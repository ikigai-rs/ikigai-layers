//! A lease on take (ledger #2119): a `take lease=` gives a receipt its taker settles (`done`,
//! with an optional result) or renews before it lapses; a lapsed lease returns the tuple to the
//! space, and a late settle is refused, so a tuple has at most one result.
#![cfg(feature = "host")]
mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use ikigai_core::{Error, Time, Verb};

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
    // After, the same tuple is pending again, with its own subject, and the feed says so.
    at(&mut f, &clock, 10);
    assert!(pending(&f).contains(&offer_iri("a")), "{}", pending(&f));
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);
    let tuple = source(
        &f.kernel,
        TUPLES,
        &[("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    );
    assert!(tuple.contains("Do a."));
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
    assert_eq!(kinds(&f), ["out", "taken", "returned", "taken", "returned"]);
    // The lapsed receipts stay, each with its attempt and state.
    let turtle = source(&f.kernel, &first, &[], &tasker(&bobby_gonk()));
    assert!(turtle.contains("\"lapsed\""), "{turtle}");
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
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

/// The host returns a lapsed tuple before every request on the layer, a read included
/// (ledger #3985): a Source of the layer's definition at the lapse appends the `returned`
/// event to the feed. The log file is read directly, so no second request can be what swept.
#[test]
fn a_definition_read_at_the_lapse_returns_the_tuple() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let log = f.state.path().join("tasks-bobby/feed.log");
    let before = std::fs::read(&log).unwrap();
    // Before the lapse a definition read leaves the feed as it was.
    at(&mut f, &clock, 9);
    source(
        &f.kernel,
        "urn:layer:tasks-bobby",
        &[],
        &tasker(&bobby_gonk()),
    );
    assert_eq!(std::fs::read(&log).unwrap(), before);
    // At the lapse the same read returns the tuple, and the feed logs it.
    at(&mut f, &clock, 10);
    source(
        &f.kernel,
        "urn:layer:tasks-bobby",
        &[],
        &tasker(&bobby_gonk()),
    );
    let after = std::fs::read(&log).unwrap();
    assert!(
        after.len() > before.len(),
        "the definition read did not sweep"
    );
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);
}
