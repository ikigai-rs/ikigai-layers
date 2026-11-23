//! A lease on take (ledger #2119): a `take lease=` gives a receipt its taker settles (`done`,
//! with an optional result) or renews before it lapses; a lapsed lease returns the tuple to the
//! space, and a late settle is refused, so a tuple has at most one result.
#![cfg(feature = "host")]
mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use ikigai_core::{Clock, Error, Time, Verb};

const MINUTE: u64 = 60_000;
const START: u64 = 1_791_590_400_000;

struct ManualClock(AtomicU64);
impl ikigai_core::Clock for ManualClock {
    fn now(&self) -> Time {
        Time::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// The task layer, defined with `lease_max = "1h"`, over a clock the test moves.
fn leased() -> (Fixture, Arc<ManualClock>) {
    let mut f = fixture();
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(&path).unwrap();
    f.config.write(
        "layers/tasks-bobby.toml",
        &format!("{definition}lease_max = \"1h\"\n"),
    );
    f.reopen();
    let clock = Arc::new(ManualClock(AtomicU64::new(START)));
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
    (f, clock)
}

fn at(f: &mut Fixture, clock: &Arc<ManualClock>, minutes: u64) {
    clock.0.store(START + minutes * MINUTE, Ordering::SeqCst);
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
}

fn take_leased(f: &Fixture, who: &str, subject: &str, lease: &str) -> Result<String, Error> {
    issue(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", subject), ("lease", lease)],
        &tasker(who),
    )
    .map(|r| text(&r))
}

/// The newest receipt of `subject`, by its IRI, from the JSON list of receipts.
fn receipt_of(f: &Fixture, subject: &str) -> String {
    let listed: serde_json::Value = serde_json::from_str(&source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("as", "application/json")],
        &tasker(&bobby_gonk()),
    ))
    .unwrap();
    listed["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|r| r["subject"] == subject)
        .map(|r| r["receipt"].as_str().unwrap().to_string())
        .unwrap_or_else(|| panic!("no receipt of {subject}: {listed}"))
}

fn settle(f: &Fixture, who: &str, receipt: &str, args: &[(&str, &str)]) -> Result<String, Error> {
    issue(&f.kernel, Verb::Sink, receipt, args, &tasker(who)).map(|r| text(&r))
}

fn pending(f: &Fixture) -> String {
    source(&f.kernel, TUPLES, &[], &tasker(&bobby_gonk()))
}

fn kinds(f: &Fixture) -> Vec<String> {
    source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &tasker(&bobby_gonk()),
    )
    .lines()
    .map(|l| l.split(' ').nth(2).unwrap().to_string())
    .collect()
}

/// The host's lease sweep, at the clock's instant, as a deployment schedules it.
fn sweep_leases(f: &Fixture, clock: &Arc<ManualClock>) {
    futures::executor::block_on(f.host.sweep_leases(clock.now())).unwrap();
}

/// A state file opened with the test's key (see [`common::sealed`]).
fn unsealed(f: &Fixture, bytes: &[u8]) -> Vec<u8> {
    let text = std::fs::read_to_string(f.config.path().join("state.key")).unwrap();
    let identity: age::x25519::Identity = text
        .lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .parse()
        .unwrap();
    age::decrypt(&identity, bytes).unwrap()
}

/// A read a test issues: its name, the resource and the arguments.
type Read<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);

/// The first lease's lapse instant in these tests: a 10m lease taken at the start.
const LAPSE: &str = "2026-10-10T00:10:00.000Z";

fn attempt_of(receipt_turtle: &str) -> String {
    triples(receipt_turtle)
        .iter()
        .find(|t| t.predicate.as_str() == "https://ikigai-rs.dev/ns/layers#attempt")
        .map(|t| t.object.to_string())
        .unwrap_or_else(|| panic!("no ik:attempt in {receipt_turtle}"))
}

#[test]
fn a_lapsed_lease_returns_the_tuple_to_the_space() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    assert!(!pending(&f).contains(&offer_iri("a")));
    // Before the lease lapses it is still taken.
    at(&mut f, &clock, 9);
    assert!(!pending(&f).contains(&offer_iri("a")));
    let refused = take(&f.kernel, &participant("bobby"), &offer_iri("a"));
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    // After, the same tuple reads as pending again, with its own subject. A read records
    // nothing (ledger #4075): the feed says so once the sweep runs.
    at(&mut f, &clock, 10);
    assert!(pending(&f).contains(&offer_iri("a")), "{}", pending(&f));
    assert_eq!(kinds(&f), ["out", "taken"]);
    let tuple = source(
        &f.kernel,
        TUPLES,
        &[("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    );
    assert!(tuple.contains("Do a."));
    sweep_leases(&f, &clock);
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);
    assert!(pending(&f).contains(&offer_iri("a")), "{}", pending(&f));
    // Anyone may take it again; the new receipt counts the attempt.
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let second = receipt_of(&f, &offer_iri("a"));
    let turtle = source(&f.kernel, &second, &[], &tasker(&bobby_gonk()));
    assert_eq!(
        attempt_of(&turtle),
        "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
    assert_eq!(kinds(&f), ["out", "taken", "returned", "taken"]);
}

#[test]
fn renew_extends_a_lease_and_done_settles_it() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let receipt = receipt_of(&f, &offer_iri("a"));
    at(&mut f, &clock, 8);
    settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "renew"), ("lease", "10m")],
    )
    .unwrap();
    // Past the first lease, inside the renewed one: still taken.
    at(&mut f, &clock, 15);
    assert!(!pending(&f).contains(&offer_iri("a")));
    let turtle = source(&f.kernel, &receipt, &[], &tasker(&bobby_gonk()));
    assert!(turtle.contains("2026-10-10T00:18:00.000Z"), "{turtle}");
    // Only its taker settles it.
    let refused = settle(&f, &participant("bobby"), &receipt, &[("settle", "done")]);
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    let result = "urn:blake3:0f1e";
    settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "done"), ("result", result)],
    )
    .unwrap();
    // Settled, the take is final: no lapse returns it.
    at(&mut f, &clock, 120);
    assert!(!pending(&f).contains(&offer_iri("a")));
    assert_eq!(kinds(&f), ["out", "taken"]);
    let turtle = source(&f.kernel, &receipt, &[], &tasker(&bobby_gonk()));
    assert!(turtle.contains(result), "{turtle}");
    assert!(turtle.contains("\"done\""), "{turtle}");
    // The same done again is the one settle; another result, or a renew, is refused.
    settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "done"), ("result", result)],
    )
    .unwrap();
    for args in [
        &[("settle", "done"), ("result", "urn:blake3:other")][..],
        &[("settle", "renew"), ("lease", "10m")][..],
    ] {
        let refused = settle(&f, &bobby_gonk(), &receipt, args);
        assert!(
            matches!(refused, Err(Error::Conflict(_))),
            "{args:?}: {refused:?}"
        );
    }
    // The JSON-LD face carries the lease under the host's own terms.
    let ld: serde_json::Value = serde_json::from_str(&source(
        &f.kernel,
        &receipt,
        &[("as", "application/ld+json")],
        &tasker(&bobby_gonk()),
    ))
    .unwrap();
    let node = &ld["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@id"] == receipt.as_str())
        .unwrap_or_else(|| panic!("{ld}"))
        .clone();
    assert_eq!(node["attempt"], "1", "{ld}");
    assert_eq!(node["leaseState"], "done", "{ld}");
    assert_eq!(node["leaseResult"], result, "{ld}");
    assert_eq!(node["leaseUntil"], "2026-10-10T00:18:00.000Z", "{ld}");
    // And across a restart.
    f.reopen();
    at(&mut f, &clock, 240);
    assert!(!pending(&f).contains(&offer_iri("a")));
    let turtle = source(&f.kernel, &receipt, &[], &tasker(&bobby_gonk()));
    assert!(turtle.contains(result), "{turtle}");
}

#[test]
fn a_late_settle_is_refused() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let first = receipt_of(&f, &offer_iri("a"));
    at(&mut f, &clock, 11);
    for args in [
        &[("settle", "done"), ("result", "urn:blake3:late")][..],
        &[("settle", "renew"), ("lease", "10m")][..],
    ] {
        match settle(&f, &bobby_gonk(), &first, args) {
            Err(Error::Conflict(m)) => assert!(m.contains("lapsed"), "{m}"),
            other => panic!("{args:?}: {other:?}"),
        }
    }
    // Someone else takes it; the late taker still cannot settle, and cannot report on it.
    take_leased(&f, &participant("bobby"), &offer_iri("a"), "10m").unwrap();
    let refused = settle(&f, &bobby_gonk(), &first, &[("settle", "done")]);
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    let handback = report("Handback", "h1", &offer_iri("a"), &gonk());
    assert!(out(&f.kernel, &bobby_gonk(), &handback).is_err());
    let second = receipt_of(&f, &offer_iri("a"));
    assert_ne!(first, second);
    settle(&f, &participant("bobby"), &second, &[("settle", "done")]).unwrap();
}

#[test]
fn the_receipt_records_attempts_across_a_restart() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "5m").unwrap();
    let first = receipt_of(&f, &offer_iri("a"));
    let turtle = source(&f.kernel, &first, &[], &tasker(&bobby_gonk()));
    assert_eq!(
        attempt_of(&turtle),
        "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
    // The host stops while the lease is held, and opens after it has lapsed.
    f.reopen();
    at(&mut f, &clock, 6);
    assert!(pending(&f).contains(&offer_iri("a")));
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "5m").unwrap();
    at(&mut f, &clock, 12);
    f.reopen();
    at(&mut f, &clock, 12);
    assert!(pending(&f).contains(&offer_iri("a")));
    // Read as pending, the second lapse is not recorded until a write: open does not sweep
    // leases yet (lease sweep item 3), and a read never does (ledger #4075).
    assert_eq!(kinds(&f), ["out", "taken", "returned", "taken"]);
    // The lapsed receipts stay, each with its attempt and state.
    let turtle = source(&f.kernel, &first, &[], &tasker(&bobby_gonk()));
    assert!(turtle.contains("\"lapsed\""), "{turtle}");
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    assert_eq!(
        kinds(&f),
        ["out", "taken", "returned", "taken", "returned", "taken"]
    );
    let third = receipt_of(&f, &offer_iri("a"));
    let turtle = source(&f.kernel, &third, &[], &tasker(&bobby_gonk()));
    assert_eq!(
        attempt_of(&turtle),
        "\"3\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
    // A take with no lease stays final, and has nothing to settle.
    at(&mut f, &clock, 600);
    assert!(!pending(&f).contains(&offer_iri("a")));
    let refused = settle(&f, &bobby_gonk(), &third, &[("settle", "done")]);
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
}

#[test]
fn a_lease_is_bounded_by_the_layer() {
    let (f, _clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    for (lease, says) in [
        ("2h", "at most 1h"),
        ("0m", "positive"),
        ("ten", "such as"),
        ("10", "such as"),
    ] {
        match take_leased(&f, &bobby_gonk(), &offer_iri("a"), lease) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "lease");
                assert!(detail.contains(says), "{lease}: {detail}");
            }
            other => panic!("{lease}: {other:?}"),
        }
    }
    assert!(
        pending(&f).contains(&offer_iri("a")),
        "nothing refused is taken"
    );
    // A layer that sets no lease_max takes no lease.
    let plain = fixture();
    out(&plain.kernel, &gonk(), &offer("a")).unwrap();
    match issue(
        &plain.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", &offer_iri("a")), ("lease", "10m")],
        &tasker(&bobby_gonk()),
    ) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "lease");
            assert!(detail.contains("lease_max"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

/// A host that stopped after storing a lapse but before its tuple went back drops the tuple
/// again when it opens, and logs the lapse's `returned` event.
#[test]
fn a_lapse_whose_tuple_never_went_back_is_repaired_on_open() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "5m").unwrap();
    let receipt = receipt_of(&f, &offer_iri("a"));
    let id = receipt.rsplit(':').next().unwrap();
    let lapse = r#"{"until":"2026-10-10T00:05:00.000Z","state":"lapsed","seq":3,"time":"2026-10-10T00:05:00.000Z"}"#;
    std::fs::write(
        f.state
            .path()
            .join(TASKS)
            .join("leases")
            .join(format!("{id}.json")),
        sealed(f.config.path(), lapse.as_bytes()),
    )
    .unwrap();
    f.reopen();
    at(&mut f, &clock, 6);
    assert!(pending(&f).contains(&offer_iri("a")), "{}", pending(&f));
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);
    let refused = settle(&f, &bobby_gonk(), &receipt, &[("settle", "done")]);
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "5m").unwrap();
    let turtle = source(
        &f.kernel,
        &receipt_of(&f, &offer_iri("a")),
        &[],
        &tasker(&bobby_gonk()),
    );
    assert_eq!(
        attempt_of(&turtle),
        "\"2\"^^<http://www.w3.org/2001/XMLSchema#integer>"
    );
}

/// A read never writes (ledger #4075): at the lapse, a definition read, an `rd` in each face, a
/// receipt read and a feed read each leave `feed.log` and the lease files byte-identical. The
/// reads compute the lapse instead: the tuple reads as pending and the receipt as `lapsed`, not
/// yet recorded. The files are read directly, so no request can be what changed them.
#[test]
fn a_read_at_the_lapse_writes_nothing() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let receipt = receipt_of(&f, &offer_iri("a"));
    let files = |f: &Fixture| {
        let dir = f.state.path().join(TASKS);
        let mut leases: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir.join("leases"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
            .collect();
        leases.sort();
        (std::fs::read(dir.join("feed.log")).unwrap(), leases)
    };
    let before = files(&f);
    at(&mut f, &clock, 10);
    let who = tasker(&bobby_gonk());
    let a = offer_iri("a");
    let reads: [Read; 10] = [
        ("definition", "urn:layer:tasks-bobby", &[]),
        ("shapes", "urn:layer:tasks-bobby:shapes", &[]),
        ("rd", TUPLES, &[]),
        ("rd as JSON", TUPLES, &[("as", "application/json")]),
        ("rd as Turtle", TUPLES, &[("as", "text/turtle")]),
        ("rd tuple=", TUPLES, &[("tuple", &a)]),
        ("rd state=taken", TUPLES, &[("state", "taken")]),
        (
            "rd state=taken as JSON",
            TUPLES,
            &[("state", "taken"), ("as", "application/json")],
        ),
        ("receipt", &receipt, &[]),
        ("feed", "urn:layer:tasks-bobby:feed", &[("after", "0")]),
    ];
    let mut answers = std::collections::HashMap::new();
    for (name, iri, args) in reads {
        answers.insert(name, source(&f.kernel, iri, args, &who));
        assert_eq!(files(&f), before, "the {name} read wrote");
    }
    // The reads computed the lapse: the tuple pending, the receipt lapsed and unrecorded.
    assert!(answers["rd"].contains(&offer_iri("a")), "{}", answers["rd"]);
    assert!(answers["rd as JSON"].contains(&offer_iri("a")));
    assert!(
        answers["rd as Turtle"].contains("Do a."),
        "{}",
        answers["rd as Turtle"]
    );
    assert!(
        answers["rd tuple="].contains("Do a."),
        "{}",
        answers["rd tuple="]
    );
    assert!(
        answers["receipt"].contains("\"lapsed\""),
        "{}",
        answers["receipt"]
    );
    let line = &answers["rd state=taken"];
    assert!(
        line.contains("lease=lapsed until=2026-10-10T00:10:00.000Z recorded=no"),
        "{line}"
    );
    let listed: serde_json::Value =
        serde_json::from_str(&answers["rd state=taken as JSON"]).unwrap();
    let lease = &listed["receipts"][0]["lease"];
    assert_eq!(lease["state"], "lapsed", "{listed}");
    assert_eq!(lease["recorded"], false, "{listed}");
    assert_eq!(kinds(&f), ["out", "taken"]);
    // Recorded, the receipt says so, and the plain line drops `recorded=no`.
    sweep_leases(&f, &clock);
    assert_ne!(files(&f), before);
    let listed: serde_json::Value = serde_json::from_str(&source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("as", "application/json")],
        &who,
    ))
    .unwrap();
    assert_eq!(listed["receipts"][0]["lease"]["recorded"], true, "{listed}");
    let line = source(&f.kernel, TUPLES, &[("state", "taken")], &who);
    assert!(line.contains("lease=lapsed"), "{line}");
    assert!(!line.contains("recorded="), "{line}");
    assert!(pending(&f).contains(&offer_iri("a")));
}

/// A lapse is recorded at the lease's `until`, not when the sweep noticed (ledger #4075): the
/// `returned` event's time and the lease file's lapse time are both the lapse instant.
#[test]
fn the_returned_time_is_the_lapse_instant() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let receipt = receipt_of(&f, &offer_iri("a"));
    at(&mut f, &clock, 25);
    sweep_leases(&f, &clock);
    let feed = source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &tasker(&bobby_gonk()),
    );
    let returned = feed
        .lines()
        .find(|l| l.split(' ').nth(2) == Some("returned"))
        .unwrap_or_else(|| panic!("no returned event: {feed}"));
    assert_eq!(returned.split(' ').nth(1), Some(LAPSE), "{feed}");
    let id = receipt.rsplit(':').next().unwrap();
    let file = std::fs::read(
        f.state
            .path()
            .join(TASKS)
            .join("leases")
            .join(format!("{id}.json")),
    )
    .unwrap();
    let lease: serde_json::Value = serde_json::from_slice(&unsealed(&f, &file)).unwrap();
    assert_eq!(lease["state"], "lapsed", "{lease}");
    assert_eq!(lease["time"], LAPSE, "{lease}");
    assert_eq!(lease["until"], LAPSE, "{lease}");
}

/// Two lapses are recorded in the order the leases lapsed, not the order of the takes, so the
/// `returned` times in the feed never go backward.
#[test]
fn lapses_are_recorded_in_until_order() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    out(&f.kernel, &gonk(), &offer("b")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "20m").unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("b"), "10m").unwrap();
    at(&mut f, &clock, 30);
    sweep_leases(&f, &clock);
    let feed = source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &tasker(&bobby_gonk()),
    );
    let returned: Vec<(&str, &str)> = feed
        .lines()
        .map(|l| l.split(' ').collect::<Vec<_>>())
        .filter(|l| l[2] == "returned")
        .map(|l| (l[1], l[3]))
        .collect();
    let b = offer_iri("b");
    let a = offer_iri("a");
    assert_eq!(
        returned,
        [
            ("2026-10-10T00:10:00.000Z", b.as_str()),
            ("2026-10-10T00:20:00.000Z", a.as_str())
        ],
        "{feed}"
    );
}
