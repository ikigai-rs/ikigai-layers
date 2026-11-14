//! Receipt retention, including durable aggregate sweep recovery.
#![cfg(feature = "host")]
mod common;
use common::*;
use ikigai_core::Time;

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
    assert_eq!(
        sweep.before,
        ikigai_layers::time::xsd_date_time(Time::from_millis(start + DAY))
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
    assert!(!old.exists());
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
    assert_eq!(f.host.layer(TASKS).unwrap().last_seq(), 3);
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
