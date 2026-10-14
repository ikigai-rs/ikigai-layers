//! The module recipe as one test: `ikigai-conformance` walks every resource this crate
//! binds and reports every violation at once.
//!
//! # What the suite cannot say, and this file says instead
//!
//! - **`say`, `out` and `urn:layers:me` are about their CALLER**: an entry and a tuple are
//!   attributed to the principal the door minted, and `me` answers who that is. Root holds
//!   every principal and names none, so under root each refuses every probe. Each is
//!   probed instead AS a participant, through the grant the generator writes for it
//!   (`Fixture::capability`, conformance 0.6.2, ledger #1321), and every check judges what
//!   it served: nothing about them is waived any more.
//! - **The definition, shapes and context are `pure`** for the life of the host: they are
//!   read once, when the host opens, and a host that changes them opens again.
//! - **An entry is `pure`**: it is named by an id minted once and never rewritten.
//! - **The entries, author-graph and latest reads are cacheable on the `say` thread**: the
//!   kernel cuts `urn:layer:{layer}:say` after every Sink through it, which is the only way
//!   an entry is written. `tests/channel.rs` pins the cut.
//! - **Feeds, the participants graphs, `urn:layers:me` and manifests are live**: a feed waits on the next
//!   event, and the config reads are re-read per request because enrollment is live.
//! - **`take` is not fired at all** (opted out): it is destructive, removing a tuple and
//!   writing a receipt. `tests/tuplespace.rs` pins `take`'s `text/turtle` tuple under a principal, and both
//!   verbs' exact capabilities.
//! - **A receipt is `pure`** (minted once, never rewritten) and **`rd` is cacheable on the
//!   `tuples` thread**, which the kernel cuts after every out and take through it.
//! - **`space(&host)` is HOST-named** (`SPACE-NAME`, ledger #987): its doors are the layers
//!   the configuration defines.
//! - **No `Suite::own` scope**: the kernel binds the layer space alone, as a door does, so
//!   every endpoint walked is this crate's and nothing would be inherited. The second test
//!   pins the walked list exactly, which is what an `own` scope plus an `inherited` pin
//!   would buy.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::sync::Arc;

use common::*;
use ikigai_conformance::{Fixture as Probe, Suite};
use ikigai_core::{Capability, Verb};
use ikigai_layers::grants::{generate, load_fingerprints};
use ikigai_layers::Config;

const SPACE_LABEL: &str = "ikigai_layers::space(&host)";
const LAYERS: [&str; 3] = ["notes", "talk", "talk-bobby"];

/// Who `say`s into each channel layer when the suite probes it: a participant whose
/// generated grant holds `say` there (brian only reads `notes`).
fn sayer(layer: &str) -> String {
    participant(if layer == "notes" { "chris" } else { "brian" })
}

/// The capability a door mints for `principal`: its principal plus the grant the generator
/// wrote for it over the fixture's own configuration, exactly what `tests/me.rs` and
/// `tests/door.rs` resolve as. The suite probes an action about its caller AS this
/// (`Fixture::capability`, conformance 0.6.2, ledger #1321) instead of under root.
fn minted(f: &common::Fixture, principal: &str) -> Capability {
    let loaded = Config::load(f.config.path()).expect("the fixture configuration");
    let fingerprints = load_fingerprints(f.config.path(), &loaded).expect("enrollment.toml");
    let e = generate(&loaded, &fingerprints).expect("an enrollment");
    let client = e
        .clients
        .values()
        .find(|c| c.principal == principal)
        .unwrap_or_else(|| panic!("{principal} is enrolled"));
    cap(
        principal,
        &e.grants[&client.grant]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
}

fn suite(f: &common::Fixture, entry_ids: &[(String, String)], receipt: &str) -> Suite {
    let mut suite = Suite::new()
        .host_named_space(SPACE_LABEL, Arc::new(ikigai_layers::space(&f.host)))
        // The kernel is a scratch fixture, so the write doors are held to REQUIRED too
        // (conformance 0.8.0, ledger #329): `say` and `out` without their required
        // `content` must be refused, typed. A drop wrongly accepted would write into the
        // fixture's own layers, nothing else.
        .required_on_mutating()
        .fixture(Probe::new("collab", Verb::Source).binding("name", "bobby"))
        .fixture(Probe::new("collab-participants", Verb::Source).binding("name", "bobby"))
        .fixture(Probe::new("layers-feed", Verb::Source).arg("after", "talk:0"))
        // Answered about its caller, so probed AS one: brian, through the grant his door
        // mints (two collaborations), and every check judges what it served.
        .fixture(Probe::new("layers-me", Verb::Source).capability(minted(f, &participant("brian"))))
        .live("layers-feed")
        .live("layers-participants")
        .live("collab")
        .live("collab-participants")
        .live("layers-me");
    for layer in LAYERS {
        let id = |action: &str| format!("layer-{layer}-{action}");
        let entry = &entry_ids
            .iter()
            .find(|(l, _)| l == layer)
            .expect("an entry per layer")
            .1;
        suite = suite
            .fixture(
                Probe::new(id("say"), Verb::Sink)
                    .arg("content", message("probe"))
                    .capability(minted(f, &sayer(layer))),
            )
            .fixture(Probe::new(id("feed"), Verb::Source).arg("after", "0"))
            .fixture(Probe::new(id("entry"), Verb::Source).binding("id", entry))
            .fixture(Probe::new(id("graph"), Verb::Source).binding("slug", "bobby"))
            .fixture(Probe::new(id("latest"), Verb::Source).arg("about", "urn:example:topic"))
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
        .fixture(
            Probe::new(id("tuples"), Verb::Sink)
                .arg("content", offer("probe"))
                .capability(minted(f, &gonk())),
        )
        .fixture(Probe::new(id("feed"), Verb::Source).arg("after", "0"))
        .fixture(Probe::new(id("receipt"), Verb::Source).binding("id", receipt))
        .opt_out(
            id("tuples"),
            Some(Verb::Delete),
            "take is destructive (it removes a tuple and writes a receipt), and under root, \
             which names no principal, it refuses every probe anyway (a take is attributed); \
             tests/tuplespace.rs pins its text/turtle answer, its capability and its races",
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
    let mut walked: Vec<&str> = report.walked.iter().map(String::as_str).collect();
    walked.sort_unstable();
    walked.dedup();
    let mut expected = vec![
        "collab".to_string(),
        "collab-participants".to_string(),
        "layers-feed".to_string(),
        "layers-me".to_string(),
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
