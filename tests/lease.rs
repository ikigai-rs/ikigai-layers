//! A lease on take (ledger #2119): a `take lease=` gives a receipt its taker settles (`done`,
//! with an optional result) or renews before it lapses; a lapsed lease returns the tuple to the
//! space, and a late settle is refused, so a tuple has at most one result.
#![cfg(feature = "host")]
mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use ikigai_core::{Clock, Error, Iri, Request, Resolution, Scope, Space, Time, Verb};

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
    leased_with("")
}

fn leased_with(extra: &str) -> (Fixture, Arc<ManualClock>) {
    let mut f = fixture();
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(&path).unwrap();
    f.config.write(
        "layers/tasks-bobby.toml",
        &format!("{definition}lease_max = \"1h\"\n{extra}"),
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
fn a_lease_max_alone_still_allows_a_final_take() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    let subject = offer_iri("a");
    take(&f.kernel, &bobby_gonk(), &subject).unwrap();
    at(&mut f, &clock, 120);
    sweep_leases(&f, &clock);
    assert!(!pending(&f).contains(&subject));
    assert_eq!(kinds(&f), ["out", "taken"]);
}

#[test]
fn the_take_contract_marks_the_lease_required_only_when_configured() {
    for (extra, required) in [("", false), ("lease_required = true\n", true)] {
        let (f, _) = leased_with(extra);
        let request = Request::new(Verb::Meta, Iri::parse(TUPLES).unwrap());
        let Resolution::Hit(resolved) = root_over(&f.host).resolve(&request, &Scope::empty())
        else {
            panic!("unresolved tuples endpoint");
        };
        let description = resolved.endpoint.describe();
        description.validate().expect("a valid description");
        let action = description
            .action_specs()
            .into_iter()
            .find(|action| action.verb == Verb::Delete)
            .expect("a take action");
        let lease = action.inputs.iter().find(|input| input.name == "lease");
        assert_eq!(lease.map(|input| input.required), Some(required));
    }
}

#[test]
fn a_required_lease_refuses_an_unleased_take() {
    let (f, _) = leased_with("lease_required = true\n");
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    let subject = offer_iri("a");
    let refused = take(&f.kernel, &bobby_gonk(), &subject).unwrap_err();
    let Error::InvalidArgument { name, detail } = &refused else {
        panic!("{refused:?}");
    };
    assert_eq!(name, "lease");
    assert_eq!(
        detail,
        "Layer `tasks-bobby` requires a lease on every take. Give a duration with lease=."
    );
    assert_eq!(refused.to_json()["detail"], detail.as_str());
    assert!(pending(&f).contains(&subject));
    assert_eq!(kinds(&f), ["out"]);
}

#[test]
fn a_required_lease_can_lapse_and_the_next_attempt_can_settle() {
    let (mut f, clock) = leased_with("lease_required = true\n");
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    let subject = offer_iri("a");
    take_leased(&f, &bobby_gonk(), &subject, "10m").unwrap();
    let first = receipt_of(&f, &subject);
    assert!(!pending(&f).contains(&subject));

    at(&mut f, &clock, 10);
    sweep_leases(&f, &clock);
    assert!(pending(&f).contains(&subject));
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);

    take_leased(&f, &bobby_gonk(), &subject, "10m").unwrap();
    let second = receipt_of(&f, &subject);
    assert_ne!(first, second);
    let result = "urn:blake3:0f1e";
    settle(
        &f,
        &bobby_gonk(),
        &second,
        &[("settle", "done"), ("result", result)],
    )
    .unwrap();

    at(&mut f, &clock, 20);
    sweep_leases(&f, &clock);
    assert!(!pending(&f).contains(&subject));
    assert_eq!(kinds(&f), ["out", "taken", "returned", "taken"]);
    let receipt = source(&f.kernel, &second, &[], &tasker(&bobby_gonk()));
    assert!(receipt.contains("\"done\""), "{receipt}");
    assert!(receipt.contains(result), "{receipt}");
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
    // The second lapse happened while the host was down: open records it (ledger #4077).
    assert_eq!(kinds(&f), ["out", "taken", "returned", "taken", "returned"]);
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

/// A settle's `result=` is stored with the lease and answered with every read of the
/// receipt, so it is bounded as every other write is: at most the layer's `entry_max`
/// bytes. One over is refused, and the take stays held.
#[test]
fn a_settle_result_is_bounded_by_the_layer() {
    let (f, _clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let receipt = receipt_of(&f, &offer_iri("a"));
    // The task layer sets no entry_max, so it has the default.
    let entry_max = ikigai_layers::config::DEFAULT_ENTRY_MAX;
    let result = format!("urn:blake3:{}", "0".repeat(entry_max));
    let refused = settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "done"), ("result", &result)],
    );
    assert!(
        matches!(refused, Err(Error::InvalidArgument { ref name, .. }) if name == "result"),
        "{:?}",
        refused.as_ref().map(|r| r.len())
    );
    let turtle = source(&f.kernel, &receipt, &[], &tasker(&bobby_gonk()));
    assert!(turtle.contains("\"held\""), "{turtle}");
    // One within the bound settles.
    let result = format!("urn:blake3:{}", "0".repeat(64));
    settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "done"), ("result", &result)],
    )
    .unwrap();
}

/// Reading the definition at expiry does not record a lease return (ledger #4215).
#[test]
fn definition_source_does_not_write_the_feed() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let before = lease_files(&f);

    at(&mut f, &clock, 10);
    source(
        &f.kernel,
        "urn:layer:tasks-bobby",
        &[],
        &tasker(&bobby_gonk()),
    );

    assert!(
        lease_files(&f) == before,
        "the definition read wrote lease state or the feed"
    );
}

/// A read never writes (ledger #4075): at the lapse, the shapes, an `rd` in each face, a
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
    let reads: [Read; 9] = [
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

/// A feed line's fields: its number, time, kind and IRI.
fn feed_lines(f: &Fixture) -> Vec<(String, String, String, String)> {
    source(
        &f.kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &tasker(&bobby_gonk()),
    )
    .lines()
    .map(|l| {
        let w: Vec<&str> = l.split(' ').collect();
        (w[0].into(), w[1].into(), w[2].into(), w[3].into())
    })
    .collect()
}

/// A take at the lapse, with no sweep, records the lapse first and then takes the tuple
/// (ledger #4076): the feed reads out, taken, returned, taken, and `returned` is at the
/// lapse instant. Before the lapse the same take is refused, as today.
#[test]
fn a_take_at_the_lapse_records_it_first() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    at(&mut f, &clock, 9);
    let refused = take(&f.kernel, &participant("bobby"), &offer_iri("a"));
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    assert_eq!(kinds(&f), ["out", "taken"]);
    at(&mut f, &clock, 12);
    take(&f.kernel, &participant("bobby"), &offer_iri("a")).unwrap();
    let feed = feed_lines(&f);
    let kinds: Vec<&str> = feed.iter().map(|l| l.2.as_str()).collect();
    assert_eq!(kinds, ["out", "taken", "returned", "taken"], "{feed:?}");
    assert_eq!(feed[2].1, LAPSE, "{feed:?}");
    assert_eq!(feed[3].1, "2026-10-10T00:12:00.000Z", "{feed:?}");
    assert!(feed.iter().all(|l| l.3 == offer_iri("a")), "{feed:?}");
    // The sweep, after, finds nothing left to record.
    sweep_leases(&f, &clock);
    assert_eq!(feed_lines(&f).len(), 4);
}

/// A take and the host's sweep started together on separate threads record the lapse once:
/// whichever holds the write lock first records it, and the other finds it recorded. Many
/// rounds, so both orders run.
#[test]
fn a_take_racing_the_sweep_returns_once() {
    for _ in 0..16 {
        let (mut f, clock) = leased();
        out(&f.kernel, &gonk(), &offer("a")).unwrap();
        take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
        at(&mut f, &clock, 10);
        let start = std::sync::Barrier::new(2);
        let (taken, swept) = std::thread::scope(|scope| {
            let taker = scope.spawn(|| {
                start.wait();
                take(&f.kernel, &participant("bobby"), &offer_iri("a"))
            });
            let sweeper = scope.spawn(|| {
                start.wait();
                futures::executor::block_on(f.host.sweep_leases(clock.now()))
            });
            (taker.join().unwrap(), sweeper.join().unwrap())
        });
        taken.unwrap();
        swept.unwrap();
        let feed = feed_lines(&f);
        let kinds: Vec<&str> = feed.iter().map(|l| l.2.as_str()).collect();
        assert_eq!(kinds, ["out", "taken", "returned", "taken"], "{feed:?}");
        assert_eq!(feed[2].1, LAPSE, "{feed:?}");
    }
}

/// Reads `before` for its first `reads` reads, then `after`: a clock that moves on while one
/// request runs, as a live clock does.
struct SteppingClock {
    read: AtomicU64,
    reads: u64,
    before: u64,
    after: u64,
}
impl ikigai_core::Clock for SteppingClock {
    fn now(&self) -> Time {
        let n = self.read.fetch_add(1, Ordering::SeqCst);
        Time::from_millis(if n < self.reads {
            self.before
        } else {
            self.after
        })
    }
}

/// A write records the lapses due at the instant it is stamped with, inside its own write
/// lock, not those due at some earlier reading of the clock (ledger #4076). The clock here
/// reads 1ms before the lapse for the first few reads of one request, then five minutes
/// after it: wherever in the request the clock moves on, a take of the lapsed tuple at the
/// later instant succeeds after recording the lapse, and an `out` stamped at the later
/// instant comes after the `returned`, so the feed's times never go backward.
#[test]
fn a_write_records_the_lapses_due_at_its_own_instant() {
    let lapse = START + 10 * MINUTE;
    for reads in 0..8 {
        for write in ["take", "out"] {
            let (mut f, _) = leased();
            out(&f.kernel, &gonk(), &offer("a")).unwrap();
            take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
            let clock = Arc::new(SteppingClock {
                read: AtomicU64::new(0),
                reads,
                before: lapse - 1,
                after: lapse + 5 * MINUTE,
            });
            f.kernel = kernel_over(&f.host).with_clock(clock.clone());
            let result = match write {
                "take" => take(&f.kernel, &participant("bobby"), &offer_iri("a")),
                _ => out(&f.kernel, &gonk(), &offer("b")).map(|_| String::new()),
            };
            let during = clock.read.load(Ordering::SeqCst);
            futures::executor::block_on(f.host.sweep_leases(clock.now())).unwrap();
            let feed = feed_lines(&f);
            let times: Vec<&str> = feed.iter().map(|l| l.1.as_str()).collect();
            assert!(
                times.windows(2).all(|w| w[0] <= w[1]),
                "{write}, clock moved after {reads} reads: feed times go backward: {feed:#?}"
            );
            if write == "take" {
                // Refused only if every clock read of the take's request was before the lapse.
                match result {
                    Ok(_) => assert_eq!(
                        feed.iter().map(|l| l.2.as_str()).collect::<Vec<_>>(),
                        ["out", "taken", "returned", "taken"],
                        "{feed:#?}"
                    ),
                    Err(e) => {
                        assert!(matches!(e, Error::Conflict(_)), "{e:?}");
                        assert!(
                            during <= reads,
                            "{write}, clock moved after {reads} reads: refused a take at an \
                             instant past the lapse: {e:?}"
                        );
                    }
                }
            } else {
                result.unwrap();
            }
        }
    }
}

/// [`leased`], with the layer also taking `match=` templates, under `layers_toml`.
fn leased_matching(layers_toml: &str) -> (Fixture, Arc<ManualClock>) {
    let (mut f, clock) = leased();
    let path = f.config.path().join("layers/tasks-bobby.toml");
    let definition = std::fs::read_to_string(&path).unwrap();
    f.config.write(
        "layers/tasks-bobby.toml",
        &format!("{definition}match = true\n"),
    );
    f.config.write("layers.toml", layers_toml);
    f.reopen();
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
    (f, clock)
}

fn rd_match(f: &Fixture, template: &str) -> Result<String, Error> {
    issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("match", template)],
        &tasker(&bobby_gonk()),
    )
    .map(|r| text(&r))
}

/// The layer's feed and its lease files, byte for byte.
fn lease_files(f: &Fixture) -> (Vec<u8>, Vec<(String, Vec<u8>)>) {
    let dir = f.state.path().join(TASKS);
    let mut leases: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir.join("leases"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| (p.display().to_string(), std::fs::read(&p).unwrap()))
        .collect();
    leases.sort();
    (std::fs::read(dir.join("feed.log")).unwrap(), leases)
}

/// Ledger #4081: a tuple taken under a lease, at the lapse instant and not yet recorded, is
/// pending to `rd match=` as it is to `rd`. Its bytes are in its receipt, not in intray, so
/// before this change a template it satisfies omitted it. The template is evaluated against
/// the receipt's copy by intray's matcher, and the read writes nothing.
#[test]
fn rd_match_at_the_lapse_lists_a_lapsed_unrecorded_tuple() {
    let (mut f, clock) = leased_matching("");
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    out(&f.kernel, &gonk(), &offer("b")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    let title =
        |name: &str| format!("ASK {{ ?s <http://purl.org/dc/terms/title> \"Task {name}\" }}");
    let offers = format!("ASK {{ ?s a <{IK}Offer> }}");
    // Held: the taken tuple matches nothing.
    at(&mut f, &clock, 9);
    assert_eq!(rd_match(&f, &title("a")).unwrap(), "");
    let before = lease_files(&f);
    at(&mut f, &clock, 10);
    let hit = rd_match(&f, &title("a")).unwrap();
    assert_eq!(hit.lines().count(), 1, "{hit}");
    assert!(
        hit.starts_with("1 ") && hit.contains(&offer_iri("a")),
        "{hit}"
    );
    assert_eq!(rd_match(&f, &title("zzz")).unwrap(), "");
    let other = rd_match(&f, &title("b")).unwrap();
    assert_eq!(other.lines().count(), 1, "{other}");
    assert!(other.contains(&offer_iri("b")), "{other}");
    // Merged with the stored hits in `rd`'s order, by `ik:seq`, and the same lines as `rd`.
    let both = rd_match(&f, &offers).unwrap();
    assert_eq!(both, pending(&f), "{both}");
    let subjects: Vec<&str> = both.lines().map(|l| l.split(' ').nth(2).unwrap()).collect();
    assert_eq!(subjects, [offer_iri("a"), offer_iri("b")]);
    let page = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("match", &offers), ("after", "1")],
        &tasker(&bobby_gonk()),
    )
    .map(|r| text(&r))
    .unwrap();
    assert_eq!(page.lines().count(), 1, "{page}");
    assert!(page.contains(&offer_iri("b")), "{page}");
    assert_eq!(lease_files(&f), before, "rd match= wrote");
    assert_eq!(kinds(&f), ["out", "out", "taken"]);
    // Recorded, the tuple is in intray again, and matched there once.
    sweep_leases(&f, &clock);
    let hit = rd_match(&f, &title("a")).unwrap();
    assert_eq!(hit.lines().count(), 1, "{hit}");
    assert!(hit.contains(&offer_iri("a")), "{hit}");
}

/// Ledger #4081: a lapsed tuple's receipt copy is evaluated inside the host's one budget, not
/// beside it. With no tuple stored in intray, a template that never ends on the receipt's copy
/// times out; before this change the copy was not evaluated and the read answered empty.
#[test]
fn rd_match_over_a_lapsed_tuple_runs_within_the_budget() {
    let (mut f, clock) = leased_matching("[match]\nbudget_ms = 200\nmax_overdue = 4\n");
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    // Eight unconstrained patterns over the copy's triples, under a FILTER that never holds.
    let patterns: String = (0..8).map(|i| format!("?s{i} ?p{i} ?o{i} . ")).collect();
    let all: Vec<String> = (0..8).map(|i| format!("STR(?o{i})")).collect();
    let endless = format!(
        "ASK {{ {patterns}FILTER(CONCAT({}) = \"never\") }}",
        all.join(", ")
    );
    at(&mut f, &clock, 9);
    assert_eq!(
        rd_match(&f, &endless).unwrap(),
        "",
        "held: nothing to evaluate"
    );
    at(&mut f, &clock, 10);
    let refused = rd_match(&f, &endless);
    assert!(
        matches!(refused, Err(Error::Timeout(ref message)) if message.contains("200 ms")),
        "{refused:?}"
    );
}

/// The tuplespace layer `tasks-chris`, defined as `tasks-bobby` is (with `lease_max`), beside
/// it on the same host: two leased layers for [`Host::next_lapse`].
const CHRIS_TUPLES: &str = "urn:layer:tasks-chris:tuples";

fn chris_tasker(principal: &str) -> ikigai_core::Capability {
    cap(
        principal,
        &[
            "urn:cap:layer:read:tasks-chris",
            "urn:cap:layer:feed:tasks-chris",
            "urn:cap:layer:out:tasks-chris",
            "urn:cap:layer:take:tasks-chris",
        ],
    )
}

/// The minute after the tests' start that `time` is, for an instant on a whole minute.
fn minute_of(time: Option<Time>) -> Option<u64> {
    time.map(|t| {
        let after = t.as_millis() - START;
        assert_eq!(after % MINUTE, 0, "{t:?}");
        after / MINUTE
    })
}

/// Ledger #4077: `Host::next_lapse` names the earliest `until` of a held lease across every
/// layer, and moves when a renew, a settle or a lapse changes it.
#[test]
fn next_lapse_names_the_earliest_until() {
    let (mut f, clock) = leased();
    let definition = std::fs::read_to_string(f.config.path().join("layers/tasks-bobby.toml"))
        .unwrap()
        .replace("Tasks for Bobby", "Tasks for Chris");
    f.config.write("layers/tasks-chris.toml", &definition);
    let chris = std::fs::read_to_string(f.config.path().join("collabs/chris.toml")).unwrap();
    f.config.write(
        "collabs/chris.toml",
        &format!(
            "{chris}\n[[mount]]\nlayer = \"tasks-chris\"\nbobby-gonk = [\"read\", \"feed\", \"out\", \"take\"]\n"
        ),
    );
    f.reopen();
    at(&mut f, &clock, 0);
    assert_eq!(f.host.next_lapse(), None, "no lease is held");
    // A lease on each layer: 20m on tasks-bobby, 30m on tasks-chris.
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "20m").unwrap();
    let theirs = offer("c").replace("tasks-bobby", "tasks-chris");
    let subject = "urn:layer:tasks-chris:offer:c";
    issue(
        &f.kernel,
        Verb::Sink,
        CHRIS_TUPLES,
        &[("content", &theirs)],
        &chris_tasker(&gonk()),
    )
    .unwrap();
    issue(
        &f.kernel,
        Verb::Delete,
        CHRIS_TUPLES,
        &[("tuple", subject), ("lease", "30m")],
        &chris_tasker(&bobby_gonk()),
    )
    .unwrap();
    assert_eq!(minute_of(f.host.next_lapse()), Some(20));
    // A renew at minute 5 for 5m makes tasks-bobby's lease EARLIER, and wakes a loop waiting
    // on the host's signal to ask again.
    at(&mut f, &clock, 5);
    let receipt = receipt_of(&f, &offer_iri("a"));
    let seen = f.host.notify().generation();
    settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "renew"), ("lease", "5m")],
    )
    .unwrap();
    assert_ne!(f.host.notify().generation(), seen, "a renew wakes the loop");
    assert_eq!(minute_of(f.host.next_lapse()), Some(10));
    // A renew past tasks-chris's lease hands the next lapse to the other layer.
    at(&mut f, &clock, 8);
    settle(
        &f,
        &bobby_gonk(),
        &receipt,
        &[("settle", "renew"), ("lease", "40m")],
    )
    .unwrap();
    assert_eq!(minute_of(f.host.next_lapse()), Some(30));
    // Recorded lapsed, a lease is no longer held: the sweep at 30 leaves tasks-bobby's.
    at(&mut f, &clock, 30);
    sweep_leases(&f, &clock);
    assert_eq!(minute_of(f.host.next_lapse()), Some(48));
    // Settled done, it is no longer held either.
    settle(&f, &bobby_gonk(), &receipt, &[("settle", "done")]).unwrap();
    assert_eq!(f.host.next_lapse(), None);
}

/// Ledger #4077: a lease that lapsed while no host ran is recorded when the host opens, at its
/// `until`, before any request, read or write, reaches the layer.
#[test]
fn open_records_a_lapse_while_down() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "10m").unwrap();
    assert_eq!(minute_of(f.host.next_lapse()), Some(10));
    // The host stops at minute 5 and opens again at minute 15. Open sweeps at its own instant
    // (the system clock, after this test's 2026-10-10 lapse), as it catches up on retention.
    at(&mut f, &clock, 5);
    f.reopen();
    at(&mut f, &clock, 15);
    // Recorded by the open itself, before any request: no lease is held any more.
    assert_eq!(
        f.host.next_lapse(),
        None,
        "nothing held once it is recorded"
    );
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);
    assert_eq!(feed_lines(&f)[2].1, LAPSE, "recorded at its until, 00:10");
    assert!(pending(&f).contains(&offer_iri("a")), "{}", pending(&f));
}

/// A lapse recorded whose tuple could not go back to the space yet (the drop back failed):
/// until a later write or sweep drops it back, the tuple is out of the space but its take is
/// over, so a read reads it as pending, from its receipt, as it does a lapse not yet recorded.
/// It never reads as taken by the lapsed taker, and never disappears from the listing.
#[test]
fn a_lapse_whose_drop_back_failed_still_reads_as_pending() {
    let (mut f, clock) = leased();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    take_leased(&f, &bobby_gonk(), &offer_iri("a"), "5m").unwrap();
    // The space's inbox is a file for a moment, so the drop back fails whoever runs the test.
    let tuples = f.state.path().join(TASKS).join("tuples");
    std::fs::rename(tuples.join("inbox"), tuples.join("inbox.away")).unwrap();
    std::fs::write(tuples.join("inbox"), b"").unwrap();
    at(&mut f, &clock, 6);
    let swept = futures::executor::block_on(f.host.sweep_leases(clock.now()));
    let listed = pending(&f);
    let one = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    )
    .map(|r| text(&r));
    std::fs::remove_file(tuples.join("inbox")).unwrap();
    std::fs::rename(tuples.join("inbox.away"), tuples.join("inbox")).unwrap();
    assert!(swept.is_err(), "the drop back did not fail: {swept:?}");
    assert!(listed.contains(&offer_iri("a")), "{listed}");
    assert!(one.as_deref().is_ok_and(|t| t.contains("Do a.")), "{one:?}");
    // The next sweep drops it back, once.
    sweep_leases(&f, &clock);
    assert!(pending(&f).contains(&offer_iri("a")), "{}", pending(&f));
    assert_eq!(kinds(&f), ["out", "taken", "returned"]);
}
