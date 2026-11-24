//! A tuple supplied to intray's matcher beside the stored ones (a lapsed lease's receipt copy,
//! ledger #4081) is held to the same nesting bound as a stored tuple: one nested deeper than
//! `ikigai_rdf::MAX_TURTLE_NESTING` never matches and is never parsed, while one within it is
//! matched as a stored tuple is.

// A child of the crate's one test module over `tests/common` (`tests/match_budget/timeout.rs`):
// loading `common` twice in one crate is refused by clippy (`duplicate_mod`).
use super::common::*;

use crate::host::ModelState;

/// `:s :p` with an object of `depth` nested triple terms.
fn nested(depth: usize) -> Vec<u8> {
    format!(
        "@prefix : <urn:x:> . :s :p {}:a{} .",
        "<<( :a :b ".repeat(depth),
        " )>>".repeat(depth)
    )
    .into_bytes()
}

#[test]
fn a_supplied_tuple_keeps_the_nesting_bound() {
    let mut f = fixture();
    f.config.write(
        "layers/tasks-bobby.toml",
        "model = \"tuplespace\"\nlabel = \"Tasks\"\nshapes = \"tasks.shapes.ttl\"\n\
         context = \"tasks.context.jsonld\"\naddressed = true\nmatch = true\n",
    );
    f.reopen();
    let ModelState::Tuplespace(space) = &f.host.layers["tasks-bobby"].model else {
        panic!("a tuplespace layer")
    };
    let limit = ikigai_rdf::MAX_TURTLE_NESTING;
    let (within, beyond) = (nested(limit), nested(limit + 1));
    let (within_id, beyond_id) = (
        ikigai_intray::tuple_id(&within),
        ikigai_intray::tuple_id(&beyond),
    );
    let hits = space
        .store
        .matching_with(
            "ASK { ?s <urn:x:p> ?o }",
            vec![(within_id.clone(), within), (beyond_id, beyond)],
        )
        .unwrap();
    assert_eq!(hits, [within_id]);
}
