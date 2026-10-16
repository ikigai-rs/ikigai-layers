//! ONE time budget for every tuplespace layer's `rd match=` (ledger #1362).
//!
//! The claim (from ledger #1327, ikigai-rs/ikigai-cli PR 471): the host built one intray
//! `SpaceEndpoint` per tuplespace layer, each with intray's default `EvaluationBudget`, so
//! each layer kept its OWN overdue count and `max_overdue` capped the cores one layer's
//! templates could hold, not the host's: N layers opted in to `match` could pin N times the
//! cap. Reproduced with the budget read from `layers.toml` but built once PER LAYER (the
//! shape of the code before this change): a template overdue in `tasks-bobby` did not stop
//! one in `tasks-two`, which answered `Ok` at `max_overdue = 1`. Now the host builds one
//! budget and every layer gets a clone, so the second is refused (`Unavailable`).
//!
//! ★ **Nothing here hangs on the machine's speed** (ledger #1685). The overdue evaluation
//! that fills the host's cap is HELD until the test releases it ([`hold_overdue`]): a real
//! template ends some time after its token (about 210 ms at k=7 in a release build on
//! plasma), so a read "right after" its timeout was a race a descheduled test thread could
//! lose. A refusal "at once" is told from a wait for a slot by giving the refused read a
//! budget it would have waited out ([`WOULD_WAIT`]), not by a 50 ms stopwatch. The one real
//! template that must outlast the budget runs against a 10 ms budget, which k=7 outlasts
//! about 25 times over in a release build.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use common::*;
use ikigai_core::{Capability, Error};
use ikigai_layers::{config, Host};
use ikigai_store::budget::{cap_budget, EvaluationBudget, TimeBudget};

const TWO: &str = "tasks-two";

/// The fixture with `tasks-bobby` and a second tuplespace layer `tasks-two` opted in to
/// templates, and the host's `layers.toml` saying `extra`.
fn two_matching_layers(extra: &str) -> Fixture {
    let mut f = fixture();
    let def = |label: &str, addressed: bool| {
        format!(
            "model = \"tuplespace\"\nlabel = \"{label}\"\nshapes = \"tasks.shapes.ttl\"\n\
             context = \"tasks.context.jsonld\"\naddressed = {addressed}\nmatch = true\n"
        )
    };
    f.config
        .write("layers/tasks-bobby.toml", &def("Tasks for Bobby", true));
    f.config
        .write(&format!("layers/{TWO}.toml"), &def("Tasks two", false));
    f.config.write(config::HOST_FILE, extra);
    f.reopen();
    f
}

/// Every verb on `tasks-two`.
fn two_tasker(principal: &str) -> Capability {
    Capability::scoped(scopes(TWO))
        .with_principal(principal)
        .expect("a principal")
}

/// Every verb on `layer`.
fn scopes(layer: &str) -> Vec<String> {
    ["read", "feed", "out", "take"]
        .map(|verb| format!("urn:cap:layer:{verb}:{layer}"))
        .to_vec()
}

/// The budget a read is given when the test must tell "refused at once" from "waited for a
/// slot": a read that waits does so for its whole budget, so one refused in a fraction of
/// this was refused without waiting, on a machine of any speed and under any load. Ordinary
/// reads beside the host's small budget get it too, so a slow machine does not time them out.
const WOULD_WAIT: Duration = Duration::from_secs(60);

/// `bobby`, holding every verb on `layer` and a budget grant of [`WOULD_WAIT`].
fn patient(layer: &str) -> Capability {
    let mut scopes = scopes(layer);
    scopes.push(cap_budget(WOULD_WAIT.as_millis() as u64));
    Capability::scoped(scopes)
        .with_principal(&participant("bobby"))
        .expect("a principal")
}

/// `k` unconstrained triple patterns (a cross product, |tuple|^k rows) under a FILTER over
/// every variable, so the ASK can never stop early: intray's `tests/match_time_budget.rs`
/// shape, whose k=7 runs on well past a 50 ms token (oxigraph checks it only where it
/// touches the dataset).
fn template(k: usize) -> String {
    let patterns: String = (0..k).map(|i| format!("?s{i} ?p{i} ?o{i} . ")).collect();
    let all: Vec<String> = (0..k)
        .map(|i| format!("STR(?s{i}), STR(?p{i}), STR(?o{i})"))
        .collect();
    format!(
        "ASK {{ {patterns}FILTER(CONCAT({}) = \"never\") }}",
        all.join(", ")
    )
}

/// `rd match=` on `layer`, by `caller`.
fn rd_match(
    f: &Fixture,
    layer: &str,
    template: &str,
    caller: &Capability,
) -> Result<String, Error> {
    issue(
        &f.kernel,
        ikigai_core::Verb::Source,
        &format!("urn:layer:{layer}:tuples"),
        &[("match", template)],
        caller,
    )
    .map(|r| text(&r))
}

/// Fill one of `budget`'s overdue places with an evaluation that cannot end until the test
/// says so: its caller is answered with the budget's `Timeout` (the work waits to be
/// released, so it can only time out), and it stays overdue until the returned sender is
/// used or dropped. This runs on the HOST's budget directly, so a layer that refuses a match
/// because of it is admitting matches against the host's one budget and no other.
fn hold_overdue(budget: &EvaluationBudget, says: &str) -> Sender<()> {
    let (release, held) = std::sync::mpsc::channel::<()>();
    let held_for = tasker(&gonk());
    match budget.run("ASK {}", &held_for, None, move |_| {
        let _ = held.recv();
        Ok(())
    }) {
        Err(Error::Timeout(message)) => assert!(message.contains(says), "{message}"),
        other => panic!("a held evaluation can only time out, got {other:?}"),
    }
    assert_eq!(
        budget.overdue_evaluations(),
        1,
        "the held evaluation is overdue"
    );
    release
}

fn assert_refused_at_the_cap(result: Result<String, Error>, start: Instant, layer: &str) {
    match result {
        Err(Error::Unavailable(message)) => assert!(
            message.contains("the most it allows is 1"),
            "{layer}: the refusal says why: {message}"
        ),
        other => panic!("{layer}: expected Unavailable at the host-wide cap, got {other:?}"),
    }
    // A read that waited for a slot would have waited out WOULD_WAIT.
    let waited = start.elapsed();
    assert!(
        waited < WOULD_WAIT / 2,
        "{layer}: refused at once, not after waiting for a slot: {waited:?}"
    );
}

/// Wall time, for how long a caller waited: a budget is real time, not a kernel `Clock`'s.
#[allow(clippy::disallowed_methods)]
fn now() -> Instant {
    Instant::now()
}

/// Wait (bounded) until no template is overdue.
fn settles(budget: &EvaluationBudget) {
    let start = now();
    while budget.overdue_evaluations() != 0 {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "an overdue template was still running after 120 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The host's one budget: an evaluation overdue on it refuses a template in EVERY layer at
/// `max_overdue = 1`, at once, and once it ends each layer answers again. And a real template
/// in a layer runs within that budget, timed out at the 10 ms `layers.toml` sets.
#[test]
fn an_overdue_match_in_one_layer_refuses_a_match_in_another_at_the_cap() {
    let f = two_matching_layers("[match]\nbudget_ms = 10\nmax_overdue = 1\n");
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    let two = format!(
        "@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
         <urn:layer:{TWO}:offer:b> a ik:Offer ; dcterms:title \"Task b\" ; ik:brief \"Do b.\" .\n"
    );
    issue(
        &f.kernel,
        ikigai_core::Verb::Sink,
        &format!("urn:layer:{TWO}:tuples"),
        &[("content", &two)],
        &two_tasker(&gonk()),
    )
    .expect("an offer in tasks-two");
    let budget = f.host.match_budget();

    // A real template in a layer is bounded by the host's budget, and named by it.
    match rd_match(
        &f,
        "tasks-bobby",
        &template(7),
        &tasker(&participant("bobby")),
    ) {
        Err(Error::Timeout(message)) => {
            assert!(message.contains("10 ms"), "names the budget: {message}")
        }
        other => panic!("expected a Timeout at 10 ms, got {other:?}"),
    }
    settles(budget);

    // One evaluation overdue on the host's budget, held there: a template in each layer is
    // refused at once. Before (a budget per layer): `Ok("")`, since neither layer's own
    // budget had anything overdue.
    let release = hold_overdue(budget, "10 ms");
    for layer in [TWO, "tasks-bobby"] {
        let start = now();
        assert_refused_at_the_cap(
            rd_match(&f, layer, &template(2), &patient(layer)),
            start,
            layer,
        );
    }
    assert_eq!(budget.overdue_evaluations(), 1, "refusing started nothing");

    // Released, it ends, and both layers answer again.
    drop(release);
    settles(budget);
    assert_eq!(
        rd_match(&f, TWO, &template(2), &patient(TWO)),
        Ok(String::new())
    );
    let ask = "ASK { ?o <http://purl.org/dc/terms/title> \"Task b\" }";
    let hits = rd_match(&f, TWO, ask, &patient(TWO)).expect("a legitimate template answers");
    assert!(hits.contains(&format!("urn:layer:{TWO}:offer:b")), "{hits}");
    let ask = "ASK { ?o <http://purl.org/dc/terms/title> \"Task a\" }";
    let hits = rd_match(&f, "tasks-bobby", ask, &patient("tasks-bobby"))
        .expect("a legitimate template answers");
    assert!(hits.contains(&offer_iri("a")), "{hits}");
}

/// What `layers.toml` sets is the budget the host serves under, and what it does not set
/// keeps ikigai-store's default.
#[test]
fn layers_toml_sets_the_hosts_budget() {
    let f = two_matching_layers("[match]\nbudget_ms = 1500\nmax_overdue = 3\n");
    let budget = f.host.match_budget().time_budget();
    assert_eq!(budget.base(), Duration::from_millis(1500));
    assert_eq!(budget.max_overdue(), 3);

    let defaults = TimeBudget::default();
    for (text, base, max_overdue) in [
        ("", defaults.base(), defaults.max_overdue()),
        ("[match]\n", defaults.base(), defaults.max_overdue()),
        ("[match]\nmax_overdue = 2\n", defaults.base(), 2),
        (
            "[match]\nbudget_ms = 250\n",
            Duration::from_millis(250),
            defaults.max_overdue(),
        ),
    ] {
        f.config.write(config::HOST_FILE, text);
        let read = config::load_match_budget(f.config.path()).expect(text);
        assert_eq!(
            (read.base(), read.max_overdue()),
            (base, max_overdue),
            "{text:?}"
        );
    }
    f.config.remove(config::HOST_FILE);
    assert_eq!(
        config::load_match_budget(f.config.path()).unwrap(),
        TimeBudget::default(),
        "no file is every default"
    );
}

/// A budget no template could live within, a cap of none, and a key the host does not know
/// stop the host at open, naming the file: never rounded, never ignored.
#[test]
fn a_budget_the_host_cannot_serve_is_refused_at_open() {
    let f = fixture();
    for (text, says) in [
        ("[match]\nbudget_ms = 0\n", "match.budget_ms"),
        ("[match]\nmax_overdue = 0\n", "match.max_overdue"),
        ("[match]\nbudget = 5\n", "budget"),
        ("[matching]\nbudget_ms = 5\n", "matching"),
        ("[match]\nbudget_ms = -1\n", "budget_ms"),
    ] {
        f.config.write(config::HOST_FILE, text);
        let state = Scratch::new("refused-state");
        let refusal = match Host::open(f.config.path(), state.path()) {
            Ok(_) => panic!("{text:?} should stop the host"),
            Err(e) => e.to_string(),
        };
        assert!(
            refusal.contains(config::HOST_FILE) && refusal.contains(says),
            "{text:?}: {refusal}"
        );
    }
}
