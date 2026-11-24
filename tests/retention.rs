//! Receipt retention, including durable aggregate sweep recovery.
#![cfg(feature = "host")]
mod common;
use common::*;
use ikigai_core::{Clock, Time, Verb};

const DAY: u64 = 86_400_000;
const START: u64 = 1_791_590_400_000;

fn receipts(f: &Fixture) -> Vec<std::path::PathBuf> {
    std::fs::read_dir(f.state.path().join(TASKS).join("receipts"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "ttl"))
        .collect()
}

struct ManualClock(std::sync::atomic::AtomicU64);
impl ikigai_core::Clock for ManualClock {
    fn now(&self) -> Time {
        Time::from_millis(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[test]
fn old_receipts_expire_new_and_pending_tuples_stay_and_the_feed_is_aggregate() {
    let mut f = fixture();
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(&path).unwrap();
    f.config.write(
        "layers/tasks-bobby.toml",
        &format!("{definition}retain = \"30d\"\n"),
    );
    f.reopen();
    let start = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let clock = std::sync::Arc::new(ManualClock(std::sync::atomic::AtomicU64::new(start)));
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    out(&f.kernel, &gonk(), &offer("pending")).unwrap();
    let old = receipts(&f).pop().unwrap();
    // A new receipt is inside the window; the old one is exactly at its boundary.
    clock
        .0
        .store(start + 29 * DAY, std::sync::atomic::Ordering::SeqCst);
    out(&f.kernel, &gonk(), &offer("new")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("new")).unwrap();
    let before = f.host.layer(TASKS).unwrap().last_seq();
    futures::executor::block_on(f.host.sweep_retention(Time::from_millis(start + 30 * DAY)))
        .unwrap();
    assert!(old.exists());
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), before);
    futures::executor::block_on(f.host.sweep_retention(Time::from_millis(start + 31 * DAY)))
        .unwrap();
    assert!(!old.exists());
    assert_eq!(receipts(&f).len(), 1);
    let events = f.host.layer(TASKS).unwrap().events_after(before);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "sweep");
    let sweep = events[0].sweep.as_ref().unwrap();
    assert_eq!(sweep.count, 1);
    // Recorded at the instant the receipt expired, the millisecond after its take plus the
    // window, not at the sweep's day 31 (ledger #4082); `before` reads as a sweep at that
    // instant would have.
    assert_eq!(
        sweep.before,
        ikigai_layers::time::xsd_date_time(Time::from_millis(start + 1))
    );
    assert_eq!(
        events[0].time,
        ikigai_layers::time::xsd_date_time(Time::from_millis(start + 30 * DAY + 1))
    );
    assert!(!serde_json::to_string(&events)
        .unwrap()
        .contains(&offer_iri("old")));
    assert!(source(&f.kernel, TUPLES, &[], &tasker(&bobby_gonk())).contains(&offer_iri("pending")));
    let taken = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken")],
        &tasker(&bobby_gonk()),
    );
    assert!(taken.contains(&offer_iri("new")));
    assert!(!taken.contains(&offer_iri("old")));
    let plain = source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", &before.to_string())],
        &tasker(&bobby_gonk()),
    );
    assert!(plain.contains("count=1 before="));
    let json = source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", &before.to_string()), ("as", "application/json")],
        &tasker(&bobby_gonk()),
    );
    assert!(json.contains("\"count\":1"));
    f.reopen();
    assert_eq!(receipts(&f).len(), 1);
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), before + 1);
    assert!(
        out(&f.kernel, &gonk(), &offer("old")).is_err(),
        "expired subjects stay used"
    );
}

#[test]
fn a_layer_without_retain_keeps_every_receipt() {
    let f = fixture();
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    let old = receipts(&f).pop().unwrap();
    let seq = f.host.layer(TASKS).unwrap().last_seq();
    futures::executor::block_on(
        f.host
            .sweep_retention(Time::from_millis(START + 1000 * DAY)),
    )
    .unwrap();
    assert!(old.exists());
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), seq);
}

fn enable_retention(f: &mut Fixture) {
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(&path).unwrap();
    f.config.write(
        "layers/tasks-bobby.toml",
        &format!("{definition}retain = \"30d\"\n"),
    );
    f.reopen();
}

#[test]
fn opening_a_host_sweeps_expired_receipts() {
    let mut f = fixture();
    // Make the age independent of the wall clock used by Host::open.
    let clock = std::sync::Arc::new(ManualClock(std::sync::atomic::AtomicU64::new(0)));
    f.kernel = kernel_over(&f.host).with_clock(clock);
    out(&f.kernel, &gonk(), &offer("ancient")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("ancient")).unwrap();
    enable_retention(&mut f);
    let events = f.host.layer(TASKS).unwrap().events_after(2);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "sweep");
    // At the instant it expired while no host had retention on, not the open's instant.
    assert_eq!(events[0].time, "1970-01-31T00:00:00.001Z");
    assert!(!source(
        &f.kernel,
        TUPLES,
        &[("state", "taken")],
        &tasker(&bobby_gonk())
    )
    .contains(&offer_iri("ancient")));
}

#[test]
fn an_unwritable_feed_prevents_deletion_and_an_interrupted_sweep_replays_once() {
    let mut f = fixture();
    enable_retention(&mut f);
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    let old = receipts(&f).pop().unwrap();
    let bytes = std::fs::read(&old).unwrap();
    let log = f.state.path().join(TASKS).join("feed.log");
    let saved_log = log.with_extension("saved");
    std::fs::rename(&log, &saved_log).unwrap();
    std::fs::create_dir(&log).unwrap();
    assert!(futures::executor::block_on(
        f.host
            .sweep_retention(Time::from_millis(START + 1000 * DAY))
    )
    .is_err());
    assert_eq!(std::fs::read(&old).unwrap(), bytes);
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 2);
    std::fs::remove_dir(&log).unwrap();
    std::fs::rename(&saved_log, &log).unwrap();
    futures::executor::block_on(
        f.host
            .sweep_retention(Time::from_millis(START + 1000 * DAY)),
    )
    .unwrap();
    assert!(!old.exists());
    // Restore the pre-delete file to simulate a stop after the aggregate was synced.
    std::fs::write(&old, bytes).unwrap();
    f.reopen();
    assert!(!old.exists());
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 3);
    assert_eq!(
        f.host.layer(TASKS).unwrap().events_after(2)[0]
            .sweep
            .as_ref()
            .unwrap()
            .count,
        1
    );
}

#[test]
fn a_staged_taken_receipt_is_deleted_too() {
    let mut f = fixture();
    enable_retention(&mut f);
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    let old = receipts(&f).pop().unwrap();
    let staged = old.parent().unwrap().join(".taking").join(format!(
        "test.{}",
        old.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::rename(&old, &staged).unwrap();
    futures::executor::block_on(
        f.host
            .sweep_retention(Time::from_millis(START + 1000 * DAY)),
    )
    .unwrap();
    assert!(!staged.exists());
    f.reopen();
    assert!(receipts(&f).is_empty());
}

#[test]
fn requests_sweep_and_replay_does_not_touch_later_takes_with_an_earlier_clock() {
    let mut f = fixture();
    enable_retention(&mut f);
    let clock = std::sync::Arc::new(ManualClock(std::sync::atomic::AtomicU64::new(0)));
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    let old = receipts(&f).pop().unwrap();
    let bytes = std::fs::read(&old).unwrap();
    let id = old.file_stem().unwrap().to_str().unwrap();
    let receipt_iri = format!("urn:layer:tasks-bobby:receipt:{id}");
    source(&f.kernel, &receipt_iri, &[], &tasker(&bobby_gonk()));
    source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("old"))],
        &tasker(&bobby_gonk()),
    );
    source(
        &f.kernel,
        TUPLES,
        &[("state", "taken")],
        &tasker(&bobby_gonk()),
    );
    clock.0.store(31 * DAY, std::sync::atomic::Ordering::SeqCst);
    source(
        &f.kernel,
        TUPLES,
        &[("state", "taken")],
        &tasker(&bobby_gonk()),
    );
    // A read past the cutoff computes the expiry and records nothing (ledger #4082)...
    assert!(old.exists());
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 2);
    assert!(issue(
        &f.kernel,
        ikigai_core::Verb::Source,
        &receipt_iri,
        &[],
        &tasker(&bobby_gonk())
    )
    .is_err());
    assert!(issue(
        &f.kernel,
        ikigai_core::Verb::Source,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("old"))],
        &tasker(&bobby_gonk())
    )
    .is_err());
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 2);
    // ...and the host's sweep records it, at the instant it expired.
    futures::executor::block_on(f.host.sweep_retention(clock.now())).unwrap();
    assert!(!old.exists());
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 3);
    assert_eq!(
        f.host.layer(TASKS).unwrap().events_after(2)[0].time,
        "1970-01-31T00:00:00.001Z"
    );
    clock.0.store(0, std::sync::atomic::Ordering::SeqCst);
    out(&f.kernel, &gonk(), &offer("later")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("later")).unwrap();
    // Replay the committed batch after disabling future sweeps. Its sequence boundary
    // protects the later receipt even though the new take's clock preceded the cutoff.
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(path)
        .unwrap()
        .replace("retain = \"30d\"\n", "");
    f.config.write("layers/tasks-bobby.toml", &definition);
    std::fs::write(&old, bytes).unwrap();
    f.reopen();
    assert!(!old.exists());
    assert_eq!(receipts(&f).len(), 1);
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 5);
    assert!(source(
        &f.kernel,
        TUPLES,
        &[("state", "taken")],
        &tasker(&bobby_gonk())
    )
    .contains(&offer_iri("later")));
}

/// The task layer with `retain = "30d"` and `extra` (such as a `lease_max`), over a clock at
/// `START` the test moves.
fn retained(extra: &str) -> (Fixture, std::sync::Arc<ManualClock>) {
    let mut f = fixture();
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(&path).unwrap();
    f.config.write(
        "layers/tasks-bobby.toml",
        &format!("{definition}retain = \"30d\"\n{extra}"),
    );
    f.reopen();
    let clock = std::sync::Arc::new(ManualClock(std::sync::atomic::AtomicU64::new(START)));
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
    (f, clock)
}

/// Move the clock to `millis` after `START`, on a fresh kernel so no cached answer hides it.
fn at(f: &mut Fixture, clock: &std::sync::Arc<ManualClock>, millis: u64) {
    clock
        .0
        .store(START + millis, std::sync::atomic::Ordering::SeqCst);
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
}

fn feed_log(f: &Fixture) -> Vec<u8> {
    std::fs::read(f.state.path().join(TASKS).join("feed.log")).unwrap()
}

fn stamp(millis: u64) -> String {
    ikigai_layers::time::xsd_date_time(Time::from_millis(START + millis))
}

/// A read past the cutoff computes the expiry (ledger #4082): the expired receipt is not
/// listed, not found by its subject or its IRI, and nothing is written, neither the feed nor
/// the receipt's deletion, whatever is read.
#[test]
fn a_read_past_the_cutoff_writes_nothing_and_hides_the_expired_receipt() {
    let (mut f, clock) = retained("");
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    let old = receipts(&f).pop().unwrap();
    let id = old.file_stem().unwrap().to_str().unwrap().to_string();
    let receipt_iri = format!("urn:layer:tasks-bobby:receipt:{id}");
    let log = feed_log(&f);
    let bobby = tasker(&bobby_gonk());
    // At the boundary instant it is still shown.
    at(&mut f, &clock, 30 * DAY);
    assert!(source(&f.kernel, TUPLES, &[("state", "taken")], &bobby).contains(&offer_iri("old")));
    at(&mut f, &clock, 30 * DAY + 1);
    let listed = source(&f.kernel, TUPLES, &[("state", "taken")], &bobby);
    assert!(!listed.contains(&offer_iri("old")), "{listed}");
    let json = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("as", "application/json")],
        &bobby,
    );
    assert!(!json.contains(&offer_iri("old")), "{json}");
    let by_subject = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("old"))],
        &bobby,
    )
    .unwrap_err();
    assert!(
        by_subject.to_string().contains("no longer in the space"),
        "{by_subject}"
    );
    assert!(issue(&f.kernel, Verb::Source, &receipt_iri, &[], &bobby).is_err());
    source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &bobby,
    );
    source(&f.kernel, "urn:layer:tasks-bobby", &[], &bobby);
    assert_eq!(feed_log(&f), log, "a read wrote the feed");
    assert!(old.exists(), "a read deleted the receipt");
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 2);
    // The deployment's next instant is the expiry, already due.
    assert_eq!(
        f.host.next_expiry(),
        Some(Time::from_millis(START + 30 * DAY + 1))
    );
}

/// The next write records the expiry first, under its write lock, at the instant the
/// receipt expired, then its own event (ledger #4082).
#[test]
fn a_write_records_the_sweep_first_at_the_expiry_time() {
    let (mut f, clock) = retained("");
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    let old = receipts(&f).pop().unwrap();
    at(&mut f, &clock, 45 * DAY);
    out(&f.kernel, &gonk(), &offer("new")).unwrap();
    assert!(!old.exists());
    let events = f.host.layer(TASKS).unwrap().events_after(2);
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(kinds, ["sweep", "out"]);
    assert_eq!(events[0].time, stamp(30 * DAY + 1));
    assert_eq!(events[0].by, "urn:layers:retention");
    let sweep = events[0].sweep.as_ref().unwrap();
    assert_eq!((sweep.count, sweep.before.as_str()), (1, stamp(1).as_str()));
    assert_eq!(events[1].time, stamp(45 * DAY));
    assert_eq!(f.host.next_expiry(), None);
}

/// A host opening records what expired while it was down, at the expiry instant, and a lapse
/// beside it in time order (ledgers #4077, #4082).
#[test]
fn open_records_what_expired_while_down() {
    let (mut f, clock) = retained("lease_max = \"1h\"\n");
    out(&f.kernel, &gonk(), &offer("old")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("old")).unwrap();
    out(&f.kernel, &gonk(), &offer("leased")).unwrap();
    at(&mut f, &clock, 30 * DAY - 30 * 60_000);
    issue(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", &offer_iri("leased")), ("lease", "1h")],
        &tasker(&bobby_gonk()),
    )
    .unwrap();
    // Open sweeps at the system clock's instant, after both.
    f.reopen();
    let events = f.host.layer(TASKS).unwrap().events_after(4);
    let got: Vec<(&str, &str)> = events
        .iter()
        .map(|e| (e.kind.as_str(), e.time.as_str()))
        .collect();
    let (expired, lapsed) = (stamp(30 * DAY + 1), stamp(30 * DAY + 30 * 60_000));
    assert_eq!(
        got,
        [("sweep", expired.as_str()), ("returned", lapsed.as_str())]
    );
}

/// Feed times never go backward when one write records lapses and retention batches due
/// at instants that interleave: each is recorded at its own instant, in that order.
#[test]
fn feed_times_stay_monotonic_across_lapses_and_retention() {
    const MINUTE: u64 = 60_000;
    let (mut f, clock) = retained("lease_max = \"1h\"\n");
    let bobby = tasker(&bobby_gonk());
    for name in ["a", "c", "leased", "d"] {
        out(&f.kernel, &gonk(), &offer(name)).unwrap();
    }
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    at(&mut f, &clock, 10 * MINUTE);
    take(&f.kernel, &bobby_gonk(), &offer_iri("c")).unwrap();
    at(&mut f, &clock, 45 * MINUTE);
    take(&f.kernel, &bobby_gonk(), &offer_iri("d")).unwrap();
    // A lease until day 30 plus 30 minutes: between c's expiry and d's.
    at(&mut f, &clock, 30 * DAY - 30 * MINUTE);
    issue(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", &offer_iri("leased")), ("lease", "1h")],
        &bobby,
    )
    .unwrap();
    let recorded = f.host.layer(TASKS).unwrap().last_seq();
    let log = feed_log(&f);
    at(&mut f, &clock, 31 * DAY);
    // A read computes all three and writes none of them.
    let pending = source(&f.kernel, TUPLES, &[], &bobby);
    assert!(pending.contains(&offer_iri("leased")), "{pending}");
    let taken = source(&f.kernel, TUPLES, &[("state", "taken")], &bobby);
    assert!(!taken.contains(&offer_iri("a")) && !taken.contains(&offer_iri("d")));
    assert_eq!(feed_log(&f), log);
    out(&f.kernel, &gonk(), &offer("e")).unwrap();
    let events = f.host.layer(TASKS).unwrap().events_after(recorded);
    let got: Vec<(&str, String)> = events
        .iter()
        .map(|e| (e.kind.as_str(), e.time.clone()))
        .collect();
    assert_eq!(
        got,
        [
            ("sweep", stamp(30 * DAY + 10 * MINUTE + 1)),
            ("returned", stamp(30 * DAY + 30 * MINUTE)),
            ("sweep", stamp(30 * DAY + 45 * MINUTE + 1)),
            ("out", stamp(31 * DAY)),
        ]
    );
    assert_eq!(events[0].sweep.as_ref().unwrap().count, 2);
    assert_eq!(events[2].sweep.as_ref().unwrap().count, 1);
    let all = f.host.layer(TASKS).unwrap().events_after(0);
    assert!(
        all.windows(2).all(|w| w[0].time <= w[1].time),
        "feed times went backward: {all:?}"
    );
}
