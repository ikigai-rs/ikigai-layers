//! `urn:layers:me` (ledger #1314): a participant learns, at the door, which principal the door
//! minted for it, what the participants graph says it is, and the collaborations that grant it
//! anything, under the GENERATED grant it already holds.

mod common;

use std::collections::BTreeSet;

use common::*;
use ikigai_core::{Capability, Error, Kernel, Verb};
use ikigai_layers::endpoints::ME;
use ikigai_layers::grants::{generate, load_fingerprints, Enrollment};
use ikigai_layers::Config;

const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
const PRIMARY_TOPIC: &str = "http://xmlns.com/foaf/0.1/primaryTopic";
const CONTRIBUTOR: &str = "http://purl.org/dc/terms/contributor";

fn enrollment(config: &Scratch) -> Enrollment {
    let loaded = Config::load(config.path()).unwrap();
    let fingerprints = load_fingerprints(config.path(), &loaded).unwrap();
    generate(&loaded, &fingerprints).unwrap()
}

/// The capability a door mints for `principal`, from the grant the generator wrote for it.
fn minted(e: &Enrollment, principal: &str) -> Capability {
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

fn me_json(kernel: &Kernel, capability: &Capability) -> serde_json::Value {
    serde_json::from_str(&source(
        kernel,
        ME,
        &[("as", "application/json")],
        capability,
    ))
    .expect("JSON")
}

/// Each triple, as `<s> <p> <o>` text.
fn facts(turtle: &str) -> BTreeSet<String> {
    triples(turtle).iter().map(ToString::to_string).collect()
}

/// Two participants, each through the grant its door mints, learn who they are: a person in
/// two collaborations and a machine acting for him in one.
#[test]
fn two_participants_each_learn_who_the_door_says_they_are() {
    let f = fixture();
    let e = enrollment(&f.config);

    let brian = minted(&e, &participant("brian"));
    let json = me_json(&f.kernel, &brian);
    assert_eq!(
        json,
        serde_json::json!({
            "principal": participant("brian"),
            "participant": participant("brian"),
            "kind": "person",
            "types": ["http://xmlns.com/foaf/0.1/Person"],
            "label": "Brian",
            "actsFor": [],
            "collaborations": ["urn:collab:bobby", "urn:collab:chris"],
        })
    );

    let machine = minted(&e, &gonk());
    let json = me_json(&f.kernel, &machine);
    assert_eq!(
        json,
        serde_json::json!({
            "principal": gonk(),
            "participant": gonk(),
            "kind": "machine",
            "types": ["https://ikigai-rs.dev/ns#Machine"],
            "label": "plasma gonk",
            "actsFor": [participant("brian")],
            "collaborations": ["urn:collab:bobby"],
        })
    );

    // The graph face says the same in RDF: the principal as the primary topic, what the
    // participants graph says of it, and each collaboration it contributes to.
    let turtle = source(&f.kernel, ME, &[], &machine);
    let expected: BTreeSet<String> = [
        format!("<{ME}> <{PRIMARY_TOPIC}> <{}>", gonk()),
        format!("<{}> <{RDF_TYPE}> <{IK}Machine>", gonk()),
        format!(
            "<{}> <http://www.w3.org/2000/01/rdf-schema#label> \"plasma gonk\"",
            gonk()
        ),
        format!(
            "<{}> <http://www.w3.org/ns/prov#actedOnBehalfOf> <{}>",
            gonk(),
            participant("brian")
        ),
        format!("<urn:collab:bobby> <{RDF_TYPE}> <{IK}Collaboration>"),
        format!("<urn:collab:bobby> <{CONTRIBUTOR}> <{}>", gonk()),
    ]
    .into_iter()
    .collect();
    assert_eq!(facts(&turtle), expected, "{turtle}");

    // No blank node, and every ik: term one the published vocabulary defines, for the
    // machine too: tests/conformance.rs checks both as brian (ledger #1321).
    for t in triples(&turtle) {
        assert!(!ikigai_layers::rdf::has_blank_node(&t), "{t}");
        for term in [t.predicate.to_string(), t.object.to_string()] {
            if let Some(local) = term.strip_prefix(&format!("<{IK}")) {
                let local = local.trim_end_matches('>');
                assert!(
                    ikigai_vocab::VOCABULARY.contains(&format!("ik:{local} a ")),
                    "ik:{local} is not in the vocabulary"
                );
            }
        }
    }
}

/// The kind word covers the third participant shape: an agent acting for a person.
#[test]
fn an_agent_is_an_agent() {
    let f = fixture();
    let e = enrollment(&f.config);
    let json = me_json(&f.kernel, &minted(&e, &participant("bobby-claude")));
    assert_eq!(json["kind"], "agent");
    assert_eq!(
        json["actsFor"],
        serde_json::json!([participant("bobby")]),
        "{json}"
    );
    assert_eq!(
        json["collaborations"],
        serde_json::json!(["urn:collab:bobby"])
    );
}

/// No grant needs a new token: every generated grant is offered `urn:layers:me`, and a
/// capability holding no collaboration token at all is not.
#[test]
fn every_generated_grant_is_offered_it_and_no_new_token_is_needed() {
    let f = fixture();
    let e = enrollment(&f.config);
    for client in e.clients.values() {
        let minted = minted(&e, &client.principal);
        let offered = f
            .kernel
            .describe_pattern(
                &f.kernel
                    .entries()
                    .unwrap()
                    .into_iter()
                    .find(|entry| {
                        f.kernel
                            .describe_pattern(&entry.pattern)
                            .is_some_and(|d| d.id == "layers-me")
                    })
                    .expect("urn:layers:me is bound")
                    .pattern,
            )
            .unwrap()
            .unsatisfied_scopes(Verb::Source, &minted)
            .is_empty();
        assert!(offered, "{} is not offered urn:layers:me", client.principal);
        assert!(
            !e.grants[&client.grant]
                .iter()
                .any(|s| s.starts_with("urn:cap:layers:")),
            "{:?}",
            e.grants[&client.grant]
        );
    }
    let layers_only = cap(&participant("bobby"), &["urn:cap:layer:read:talk"]);
    let refused = issue(&f.kernel, Verb::Source, ME, &[], &layers_only);
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
}

/// A request that names nobody is refused by name, never answered with an empty graph: a
/// grant with no principal, and root, which names nobody.
#[test]
fn no_principal_is_a_refusal() {
    let f = fixture();
    let unnamed = Capability::scoped(["urn:cap:collab:read:bobby"]);
    for capability in [unnamed, Capability::root()] {
        for face in ["text/turtle", "application/json"] {
            match issue(&f.kernel, Verb::Source, ME, &[("as", face)], &capability) {
                Err(Error::Denied(why)) => assert!(why.contains("names nobody"), "{why}"),
                other => panic!("{face}: {other:?}"),
            }
        }
    }
}

/// A principal the participants graph does not name is still told who the door says it is,
/// and that it is no participant.
#[test]
fn a_principal_that_is_no_participant_learns_only_its_principal() {
    let f = fixture();
    let stranger = cap(
        "urn:collab:participant:dana",
        &["urn:cap:collab:read:bobby"],
    );
    let json = me_json(&f.kernel, &stranger);
    assert_eq!(
        json,
        serde_json::json!({
            "principal": "urn:collab:participant:dana",
            "participant": null,
            "kind": null,
            "types": [],
            "label": null,
            "actsFor": [],
            "collaborations": [],
        })
    );
    let turtle = source(&f.kernel, ME, &[], &stranger);
    assert_eq!(
        facts(&turtle),
        [format!(
            "<{ME}> <{PRIMARY_TOPIC}> <urn:collab:participant:dana>"
        )]
        .into_iter()
        .collect(),
        "{turtle}"
    );
}

/// An attenuated capability is told only the collaborations whose tokens it still holds, so
/// `me` never says more than `urn:collab:{name}` would let it read.
#[test]
fn an_attenuated_capability_sees_only_the_collaborations_it_holds() {
    let f = fixture();
    let narrowed = cap(&participant("brian"), &["urn:cap:collab:read:chris"]);
    let json = me_json(&f.kernel, &narrowed);
    assert_eq!(json["principal"], participant("brian"));
    assert_eq!(
        json["collaborations"],
        serde_json::json!(["urn:collab:chris"])
    );
}

/// Enrollment is live: a participant added to the graph is answered as one on the next read.
#[test]
fn the_participants_graph_is_read_per_request() {
    let f = fixture();
    let dana = cap(
        "urn:collab:participant:dana",
        &["urn:cap:collab:read:bobby"],
    );
    assert_eq!(
        me_json(&f.kernel, &dana)["participant"],
        serde_json::Value::Null
    );
    f.config.write(
        "participants.ttl",
        &format!(
            "{}<urn:collab:participant:dana> a <http://xmlns.com/foaf/0.1/Person> .\n",
            participants_ttl()
        ),
    );
    let json = me_json(&f.kernel, &dana);
    assert_eq!(json["participant"], "urn:collab:participant:dana");
    assert_eq!(json["kind"], "person");
    // No manifest grants dana anything yet.
    assert_eq!(json["collaborations"], serde_json::json!([]));
}

/// An unknown face is refused by name.
#[test]
fn an_unknown_face_is_refused() {
    let f = fixture();
    let e = enrollment(&f.config);
    let brian = minted(&e, &participant("brian"));
    let refused = issue(&f.kernel, Verb::Source, ME, &[("as", "text/plain")], &brian);
    assert!(
        matches!(&refused, Err(Error::InvalidArgument { name, .. }) if name == "as"),
        "{refused:?}"
    );
}
