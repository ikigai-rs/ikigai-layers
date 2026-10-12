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

mod common;

use std::time::{Duration, Instant};

use common::*;
use ikigai_core::{Capability, Error};
use ikigai_layers::{config, Host};
use ikigai_store::budget::{EvaluationBudget, TimeBudget};

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
    cap(
        principal,
        &[
            "urn:cap:layer:read:tasks-two",
            "urn:cap:layer:feed:tasks-two",
            "urn:cap:layer:out:tasks-two",
            "urn:cap:layer:take:tasks-two",
        ],
    )
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

/// `rd match=` on `layer`.
fn rd_match(f: &Fixture, layer: &str, template: &str) -> Result<String, Error> {
    let caller = if layer == TWO {
        two_tasker(&participant("bobby"))
    } else {
        tasker(&participant("bobby"))
    };
    issue(
        &f.kernel,
        ikigai_core::Verb::Source,
        &format!("urn:layer:{layer}:tuples"),
        &[("match", template)],
        &caller,
    )
    .map(|r| text(&r))
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

/// The host's one budget: a template overdue in one layer refuses a template in ANOTHER at
/// `max_overdue = 1`, at once, and once it ends the other layer answers again.
#[test]
fn an_overdue_match_in_one_layer_refuses_a_match_in_another_at_the_cap() {
    let f = two_matching_layers("[match]\nbudget_ms = 50\nmax_overdue = 1\n");
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

    match rd_match(&f, "tasks-bobby", &template(7)) {
        Err(Error::Timeout(message)) => {
            assert!(message.contains("50 ms"), "names the budget: {message}")
        }
        other => panic!("expected a Timeout at 50 ms, got {other:?}"),
    }
    let start = now();
    // Before: `Ok("")`, since tasks-two's own budget had nothing overdue.
    match rd_match(&f, TWO, &template(2)) {
        Err(Error::Unavailable(message)) => assert!(
            message.contains("the most it allows is 1"),
            "the refusal says why: {message}"
        ),
        other => panic!("expected Unavailable at the host-wide cap, got {other:?}"),
    }
    assert!(
        start.elapsed() < Duration::from_millis(50),
        "refused at once"
    );
    assert_eq!(
        f.host.match_budget().overdue_evaluations(),
        1,
        "the host's budget counted the abandoned template"
    );
    settles(f.host.match_budget());
    assert_eq!(rd_match(&f, TWO, &template(2)), Ok(String::new()));
    let ask = "ASK { ?o <http://purl.org/dc/terms/title> \"Task b\" }";
    let hits = rd_match(&f, TWO, ask).expect("a legitimate template answers");
    assert!(hits.contains(&format!("urn:layer:{TWO}:offer:b")), "{hits}");
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
