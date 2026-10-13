//! Every shipped shapes graph fits ikigai-shacl's rudof-free SUBSET validator (ledger #1405).
//!
//! This crate links ikigai-shacl without default features, so admission runs the subset
//! validator, which refuses BY NAME any construct outside its subset rather than skipping it. A
//! shapes graph that strays outside would stop a layer at open (`Host::open` compiles the shapes)
//! and refuse every write. These tests call `ikigai_shacl::core_shacl` BY NAME, so they hold the
//! shipped shapes to the subset whatever features a build unifies: a consumer that enables
//! `ikigai-shacl/rudof` still runs them against the subset.
//!
//! Each graph must compile, and each closed shape must actually close: a targeted node with an
//! undeclared property is refused with a `sh:ClosedConstraintComponent` result naming that
//! property, while its `rdf:type` (the one ignored property) is not.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use ikigai_layers::gateway::{self, EVENT_SHAPES, SHAPES};
use ikigai_shacl::core_shacl;

const IK: &str = "https://ikigai-rs.dev/ns#";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const CLOSED: &str = "http://www.w3.org/ns/shacl#ClosedConstraintComponent";
const UNDECLARED: &str = "urn:x:undeclared";

/// Each shipped shapes graph, with every class its node shapes target.
fn shipped() -> Vec<(&'static str, &'static str, Vec<String>)> {
    let gw = |local: &str| format!("{}{local}", gateway::NS);
    let ik = |local: &str| format!("{IK}{local}");
    vec![
        (
            "gateway::SHAPES",
            SHAPES,
            vec![gw("Request"), gw("Reply"), gw("Failure")],
        ),
        (
            "gateway::EVENT_SHAPES",
            EVENT_SHAPES,
            vec![gw("Invalidation")],
        ),
        (
            "fixture SHAPES",
            common::SHAPES,
            vec![ik("Message"), ik("Question")],
        ),
        (
            "fixture TASK_SHAPES",
            common::TASK_SHAPES,
            vec![ik("Offer"), ik("Progress"), ik("Release"), ik("Handback")],
        ),
    ]
}

#[test]
fn every_shipped_shapes_graph_compiles_under_the_subset() {
    for (label, shapes, _) in shipped() {
        let report = core_shacl::validate_report("", shapes)
            .unwrap_or_else(|e| panic!("{label}: the subset validator refused the shapes: {e}"));
        assert!(report.conforms, "{label}: an empty graph must conform");
    }
}

#[test]
fn every_shipped_closed_shape_closes_under_the_subset() {
    for (label, shapes, classes) in shipped() {
        for class in classes {
            let data = format!("<urn:x:node> a <{class}> ; <{UNDECLARED}> \"x\" .\n");
            let report = core_shacl::validate_report(&data, shapes)
                .unwrap_or_else(|e| panic!("{label} / {class}: the subset refused: {e}"));
            assert!(
                !report.conforms,
                "{label} / {class}: an undeclared property was admitted"
            );
            let closed: Vec<_> = report
                .violations
                .iter()
                .filter(|v| v.component == CLOSED)
                .collect();
            assert!(
                closed.iter().any(|v| v.path.as_deref() == Some(UNDECLARED)),
                "{label} / {class}: no closed result names the undeclared property: {closed:?}"
            );
            assert!(
                closed.iter().all(|v| v.path.as_deref() != Some(RDF_TYPE)),
                "{label} / {class}: rdf:type is ignored, never a closed result: {closed:?}"
            );
        }
    }
}
