//! The module recipe as one test: `ikigai-conformance` walks every resource this crate
//! binds and reports every violation at once.
//!
//! # What the suite cannot say, and this file says instead
//!
//! - **`say` is probed under root, and root names nobody**, so every probe of it is a
//!   `Denied` from the endpoint (an entry must be attributed). The suite cannot mint a
//!   principal, so `OUTPUTS` is waived for the say doors with that reason, and
//!   `tests/channel.rs` pins what `say` answers (`text/plain`, the entry's IRI) under a
//!   principal.
//! - **The definition, shapes and context are `pure`** for the life of the host: they are
//!   read once, when the host opens, and a host that changes them opens again.
//! - **An entry is `pure`**: it is named by an id minted once and never rewritten.
//! - **The entries, author-graph and latest reads are cacheable on the `say` thread**: the
//!   kernel cuts `urn:layer:{layer}:say` after every Sink through it, which is the only way
//!   an entry is written. `tests/channel.rs` pins the cut.
//! - **Feeds, the participants graph and manifests are live**: a feed waits on the next
//!   event, and the two config reads are re-read per request because enrollment is live.
//! - **`urn:shacl:validate` is in this kernel** because admission composes it; it is
//!   ikigai-shacl's to conform, and its own suite does.
//! - **`out` is probed under root too**, so it refuses every probe the same way (a drop is
//!   attributed): `OUTPUTS` is waived for the tuples door with that reason. **`take` is not
//!   fired at all**: it is destructive, and would refuse under root for the same reason.
//!   `tests/tuplespace.rs` pins `out`'s `text/plain` subject and `take`'s `text/turtle`
//!   tuple under a principal, and both verbs' exact capabilities.
//! - **A receipt is `pure`** (minted once, never rewritten) and **`rd` is cacheable on the
//!   `tuples` thread**, which the kernel cuts after every out and take through it.
//! - **`space(&host)` is HOST-named** (`SPACE-NAME`, ledger #987): its doors are the layers
//!   the configuration defines.

mod common;

use std::sync::Arc;

use common::*;
use ikigai_conformance::{Check, Fixture as Probe, Suite};
use ikigai_core::Verb;

const SPACE_LABEL: &str = "ikigai_layers::space(&host)";
const LAYERS: [&str; 3] = ["notes", "talk", "talk-bobby"];

fn suite(f: &common::Fixture, entry_ids: &[(String, String)], receipt: &str) -> Suite {
    let mut suite = Suite::new()
        .host_named_space(SPACE_LABEL, Arc::new(ikigai_layers::space(&f.host)))
        .opt_out(
            "shacl-validate",
            None,
            "ikigai-shacl's own conformance suite covers it; it is bound here only because \
             admission composes it",
        )
        .fixture(Probe::new("collab", Verb::Source).binding("name", "bobby"))
        .fixture(Probe::new("layers-feed", Verb::Source).arg("after", "talk:0"))
        .live("layers-feed")
        .live("layers-participants")
        .live("collab");
    for layer in LAYERS {
        let id = |action: &str| format!("layer-{layer}-{action}");
        let entry = &entry_ids
            .iter()
            .find(|(l, _)| l == layer)
            .expect("an entry per layer")
            .1;
        suite = suite
            .fixture(Probe::new(id("say"), Verb::Sink).arg("content", message("probe")))
            .fixture(Probe::new(id("feed"), Verb::Source).arg("after", "0"))
            .fixture(Probe::new(id("entry"), Verb::Source).binding("id", entry))
            .fixture(Probe::new(id("graph"), Verb::Source).binding("slug", "bobby"))
            .fixture(Probe::new(id("latest"), Verb::Source).arg("about", "urn:example:topic"))
            .opt_out_check(
                id("say"),
                Check::Outputs,
                "the suite probes under root, which names no principal, so `say` refuses \
                 every probe (an entry is attributed); tests/channel.rs pins its text/plain \
                 answer under a principal",
            )
            .pure(id("definition"))
            .pure(id("shapes"))
            .pure(id("context"))
            .pure(id("entry"))
            .cacheable(id("entries"))
            .cacheable(id("graph"))
            .cacheable(id("latest"))
            .live(id("feed"));
    }
    let id = |action: &str| format!("layer-{TASKS}-{action}");
    suite
        .fixture(Probe::new(id("tuples"), Verb::Sink).arg("content", offer("probe")))
        .fixture(Probe::new(id("feed"), Verb::Source).arg("after", "0"))
        .fixture(Probe::new(id("receipt"), Verb::Source).binding("id", receipt))
        .opt_out(
            id("tuples"),
            Some(Verb::Delete),
            "take is destructive (it removes a tuple and writes a receipt), and under root, \
             which names no principal, it refuses every probe anyway (a take is attributed); \
             tests/tuplespace.rs pins its text/turtle answer, its capability and its races",
        )
        .opt_out_check(
            id("tuples"),
            Check::Outputs,
            "the suite probes under root, which names no principal, so `out` refuses every \
             probe (a drop is attributed); tests/tuplespace.rs pins its text/plain answer under \
             a principal",
        )
        .pure(id("definition"))
        .pure(id("shapes"))
        .pure(id("context"))
        .pure(id("receipt"))
        .cacheable(id("tuples"))
        .live(id("feed"))
}

fn seeded() -> (common::Fixture, Vec<(String, String)>, String) {
    let f = fixture();
    let mut ids = Vec::new();
    for layer in LAYERS {
        let about = format!(
            "@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n<urn:layer:new> a ik:Message ; ik:body \"seed\" ; dcterms:subject <urn:example:topic> .\n"
        );
        let iri = say(&f.kernel, layer, &participant("bobby"), &about).unwrap();
        ids.push((
            layer.to_string(),
            iri.rsplit(':').next().unwrap().to_string(),
        ));
    }
    out(&f.kernel, &gonk(), &offer("seed")).unwrap();
    out(&f.kernel, &gonk(), &offer("taken")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("taken")).unwrap();
    let receipts = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("taken"))],
        &tasker(&bobby_gonk()),
    );
    let receipt = receipts
        .split(|c: char| c == '<' || c == '>' || c.is_whitespace())
        .find_map(|w| w.strip_prefix("urn:layer:tasks-bobby:receipt:"))
        .expect("a receipt IRI")
        .to_string();
    (f, ids, receipt)
}

#[test]
fn conforms() {
    let (f, ids, receipt) = seeded();
    let report = suite(&f, &ids, &receipt).run_blocking(&f.kernel);
    println!("{report}");
    assert!(report.is_clean(), "{report}");
    assert!(
        report
            .to_string()
            .contains(&format!("space: {SPACE_LABEL} host-named\n")),
        "{report}"
    );
}

/// ★ A clean report over a walk that reached nothing would look exactly like a clean
/// report over a walk that reached everything. This pins the list.
#[test]
fn the_walk_reaches_every_resource_this_crate_binds() {
    let (f, ids, receipt) = seeded();
    let report = suite(&f, &ids, &receipt).run_blocking(&f.kernel);
    let mut walked: Vec<&str> = report
        .walked
        .iter()
        .map(String::as_str)
        .filter(|id| *id != "shacl-validate")
        .collect();
    walked.sort_unstable();
    walked.dedup();
    let mut expected = vec![
        "collab".to_string(),
        "layers-feed".to_string(),
        "layers-participants".to_string(),
    ];
    for layer in LAYERS {
        for action in [
            "context",
            "definition",
            "entries",
            "entry",
            "feed",
            "graph",
            "latest",
            "say",
            "shapes",
        ] {
            expected.push(format!("layer-{layer}-{action}"));
        }
    }
    for action in [
        "context",
        "definition",
        "feed",
        "receipt",
        "shapes",
        "tuples",
    ] {
        expected.push(format!("layer-{TASKS}-{action}"));
    }
    expected.sort_unstable();
    assert_eq!(walked, expected);
}
