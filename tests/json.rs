//! The JSON faces (ledger #1371): every answer a participant reads has a JSON form, so a
//! participant with only an HTTP client and a JSON parser can take part. A graph answer is
//! the layer's egress context, PRUNED and compacted, always one top-level `@graph` array; a
//! list or a write's answer is plain JSON. Every assertion here reads with `serde_json` alone:
//! no RDF parser on the reading side.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use common::*;
use ikigai_core::{Capability, Error, Kernel, Verb};
use ikigai_layers::Host;
use serde_json::Value;

const JSON: &str = "application/json";
const JSON_LD: &str = "application/ld+json";

/// Issue and parse the answer as JSON, checking its media type.
fn json(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
    capability: &Capability,
    media: &str,
) -> Value {
    let repr = issue(kernel, verb, iri, args, capability).unwrap_or_else(|e| panic!("{iri}: {e}"));
    assert_eq!(repr.repr_type.media_type, media, "{iri} {args:?}");
    serde_json::from_slice(&repr.bytes).unwrap_or_else(|e| panic!("{iri}: not JSON ({e})"))
}

/// A Source read, as JSON-LD.
fn ld(kernel: &Kernel, iri: &str, args: &[(&str, &str)], capability: &Capability) -> Value {
    let mut args = args.to_vec();
    args.push(("as", JSON_LD));
    json(kernel, Verb::Source, iri, &args, capability, JSON_LD)
}

/// The nodes of a JSON-LD answer: ALWAYS a top-level `@graph` array.
fn graph(doc: &Value) -> &Vec<Value> {
    assert!(doc.get("@context").is_some(), "no @context: {doc}");
    doc.get("@graph")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no top-level @graph array: {doc}"))
}

/// The one node of a JSON-LD answer with `@id` = `id`.
fn node<'a>(doc: &'a Value, id: &str) -> &'a Value {
    graph(doc)
        .iter()
        .find(|n| n["@id"] == id)
        .unwrap_or_else(|| panic!("no node <{id}> in {doc}"))
}

fn brian() -> String {
    participant("brian")
}

fn message_about(body: &str, about: &str) -> String {
    format!(
        "@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
         <urn:layer:new> a ik:Message ; ik:body \"{body}\" ; dcterms:subject <{about}> .\n"
    )
}

// ---- a channel ----------------------------------------------------------------------------

#[test]
fn say_answers_its_iri_as_json() {
    let f = fixture();
    let answer = json(
        &f.kernel,
        Verb::Sink,
        "urn:layer:talk:say",
        &[("content", &message("hello")), ("as", JSON)],
        &member(&brian()),
        JSON,
    );
    let iri = answer["@id"].as_str().expect("an @id string");
    assert!(iri.starts_with("urn:layer:talk:entry:"), "{answer}");
    assert_eq!(answer.as_object().unwrap().len(), 1, "{answer}");
    // The default is unchanged: the IRI as text.
    let plain = say(&f.kernel, "talk", &brian(), &message("again")).unwrap();
    assert!(plain.starts_with("urn:layer:talk:entry:"));
}

#[test]
fn an_entry_reads_as_the_compacted_context_one_node_in_a_graph() {
    let f = fixture();
    let iri = say(&f.kernel, "talk", &brian(), &message("hello")).unwrap();
    let id = iri.rsplit(':').next().unwrap();
    let doc = ld(
        &f.kernel,
        &format!("urn:layer:talk:entry:{id}"),
        &[],
        &member(&brian()),
    );
    // One node is still a one-element @graph: one shape for one node and many.
    assert_eq!(graph(&doc).len(), 1, "{doc}");
    let entry = node(&doc, &iri);
    assert_eq!(entry["@type"], "Message");
    assert_eq!(entry["body"], "hello");
    // The host's stamps, under the host's fixed terms, as lexical STRINGS.
    assert_eq!(entry["attributedTo"], brian());
    assert_eq!(entry["seq"], "1");
    let created = entry["created"].as_str().expect("created is a string");
    assert!(
        created.starts_with("2026-10-10T") && created.ends_with('Z'),
        "{created}"
    );
    // Nothing but the context's terms: no full IRI as a key.
    for key in entry.as_object().unwrap().keys() {
        assert!(
            !key.contains(':'),
            "a key that is not a term: {key} in {entry}"
        );
    }
}

#[test]
fn entries_graph_and_latest_read_as_json_ld_in_feed_order() {
    let f = fixture();
    let first = say(
        &f.kernel,
        "talk",
        &brian(),
        &message_about("one", "urn:example:topic"),
    )
    .unwrap();
    let second = say(&f.kernel, "talk", &participant("bobby"), &message("two")).unwrap();
    let third = say(
        &f.kernel,
        "talk",
        &brian(),
        &message_about("three", "urn:example:topic"),
    )
    .unwrap();

    let all = ld(&f.kernel, "urn:layer:talk:entries", &[], &member(&brian()));
    let ids: Vec<&str> = graph(&all)
        .iter()
        .map(|n| n["@id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, [first.as_str(), second.as_str(), third.as_str()]);
    let seqs: Vec<&str> = graph(&all)
        .iter()
        .map(|n| n["seq"].as_str().unwrap())
        .collect();
    assert_eq!(seqs, ["1", "2", "3"]);
    assert_eq!(node(&all, &first)["subject"], "urn:example:topic");

    let after = ld(
        &f.kernel,
        "urn:layer:talk:entries",
        &[("after", "1"), ("from", &brian())],
        &member(&brian()),
    );
    assert_eq!(graph(&after).len(), 1);
    assert_eq!(node(&after, &third)["body"], "three");

    let mine = ld(
        &f.kernel,
        "urn:layer:talk:graph:brian",
        &[],
        &member(&brian()),
    );
    assert_eq!(graph(&mine).len(), 2);

    let latest = ld(
        &f.kernel,
        "urn:layer:talk:latest",
        &[("about", "urn:example:topic")],
        &member(&brian()),
    );
    assert_eq!(graph(&latest).len(), 1);
    assert_eq!(node(&latest, &third)["body"], "three");

    // No entries is an empty @graph, not a missing one.
    let none = ld(
        &f.kernel,
        "urn:layer:talk:entries",
        &[("after", "3")],
        &member(&brian()),
    );
    assert!(graph(&none).is_empty());
}

#[test]
fn a_face_nobody_offers_is_refused_by_name() {
    let f = fixture();
    match issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk:entries",
        &[("as", "application/xml")],
        &member(&brian()),
    ) {
        Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "as"),
        other => panic!("{other:?}"),
    }
}

// ---- the prune ----------------------------------------------------------------------------

/// A layer whose shapes admit `dcterms:subject` and `ik:Question`, but whose egress context
/// names neither: what the context does not define does not cross.
#[test]
fn what_the_context_does_not_define_is_absent() {
    let config = Scratch::new("json-prune-config");
    write_config(&config);
    config.write(
        "layers/narrow.context.jsonld",
        r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#",
  "Message": "ik:Message",
  "body": "ik:body"
} }"#,
    );
    config.write(
        "layers/narrow.toml",
        "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"narrow.context.jsonld\"\n",
    );
    config.write(
        "collabs/narrow.toml",
        "[[mount]]\nlayer = \"narrow\"\nbrian = [\"read\", \"feed\", \"say\"]\n",
    );
    let state = Scratch::new("json-prune-state");
    let host = Host::open(config.path(), state.path(), &common::key(config.path()))
        .expect("the narrow layer opens");
    let kernel = kernel_over(&host);
    let who = cap(
        &brian(),
        &["urn:cap:layer:say:narrow", "urn:cap:layer:read:narrow"],
    );
    let say = |content: &str| {
        text(
            &issue(
                &kernel,
                Verb::Sink,
                "urn:layer:narrow:say",
                &[("content", content)],
                &who,
            )
            .unwrap(),
        )
    };
    let message = say(&message_about("kept", "urn:example:secret-topic"));
    let question = say(&format!(
        "@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Question ; ik:body \"asked\" .\n"
    ));

    // The Turtle face carries everything the shapes admitted.
    let turtle = source(&kernel, "urn:layer:narrow:entries", &[], &who);
    assert!(turtle.contains("secret-topic") && turtle.contains("Question"));

    let doc = ld(&kernel, "urn:layer:narrow:entries", &[], &who);
    let body = doc.to_string();
    assert!(!body.contains("secret-topic"), "{body}");
    assert!(!body.contains("purl.org/dc/terms/subject"), "{body}");
    assert!(!body.contains("Question"), "{body}");
    let kept = node(&doc, &message);
    assert_eq!(kept["body"], "kept");
    assert!(kept.get("subject").is_none(), "{kept}");
    // The undefined type is gone; the node, which has defined properties left, stays.
    let asked = node(&doc, &question);
    assert!(asked.get("@type").is_none(), "{asked}");
    assert_eq!(asked["body"], "asked");
    // And the stamps survive a context that does not name them: the host supplies its terms.
    assert_eq!(kept["attributedTo"], brian());
    assert_eq!(kept["seq"], "1");
    assert!(kept["created"].is_string());
}

#[test]
fn a_context_that_redefines_a_stamp_term_is_refused_at_open() {
    for (context, says) in [
        // The host's term, meaning something else.
        (
            r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#", "Message": "ik:Message",
               "body": "ik:body", "created": "http://purl.org/dc/terms/date" } }"#,
            "`created`",
        ),
        // The host's term, with another datatype.
        (
            r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#", "Message": "ik:Message",
               "body": "ik:body", "seq": "ik:seq" } }"#,
            "`seq`",
        ),
        // The stamp's property under a shorter name, which compaction would prefer.
        (
            r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#", "Message": "ik:Message",
               "body": "ik:body",
               "by": { "@id": "http://www.w3.org/ns/prov#wasAttributedTo", "@type": "@id" } } }"#,
            "`attributedTo`",
        ),
        // A scoped context, which the prune cannot read one allowlist from.
        (
            r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#", "Message": "ik:Message",
               "body": { "@id": "ik:body", "@context": { "x": "ik:x" } } } }"#,
            "scoped",
        ),
    ] {
        let config = Scratch::new("json-refuse");
        write_config(&config);
        config.write("layers/msg.context.jsonld", context);
        let state = Scratch::new("json-refuse-state");
        let refused = Host::open(config.path(), state.path(), &common::key(config.path()))
            .err()
            .unwrap_or_else(|| panic!("opened over {context}"))
            .to_string();
        // The first layer naming the context, in name order.
        assert!(refused.contains("layer `notes`"), "{refused}");
        assert!(refused.contains(says), "{says}: {refused}");
    }
    // The fixture's own contexts define `attributedTo`, `created` and `seq` exactly as the
    // host does: a redefinition that agrees is no redefinition, and the fixture opens.
    let _ = fixture();
}

// ---- a tuplespace -------------------------------------------------------------------------

#[test]
fn out_rd_take_and_the_receipt_all_read_as_json() {
    let f = fixture();
    let plasma = gonk();
    let taker = bobby_gonk();
    let dropped = json(
        &f.kernel,
        Verb::Sink,
        TUPLES,
        &[("content", &offer("a")), ("as", JSON)],
        &tasker(&plasma),
        JSON,
    );
    assert_eq!(dropped, serde_json::json!({ "@id": offer_iri("a") }));
    out(&f.kernel, &plasma, &offer("b")).unwrap();

    // rd's pending list, mirroring the feed's JSON.
    let pending = json(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("as", JSON)],
        &tasker(&taker),
        JSON,
    );
    assert_eq!(pending["layer"], "tasks-bobby");
    let tuples = pending["tuples"].as_array().unwrap();
    assert_eq!(tuples.len(), 2);
    assert_eq!(tuples[0]["seq"], 1);
    assert_eq!(tuples[0]["subject"], offer_iri("a"));
    assert_eq!(tuples[0]["dropper"], plasma);
    assert!(tuples[0]["created"]
        .as_str()
        .unwrap()
        .starts_with("2026-10-10T"));
    assert_eq!(tuples[1]["subject"], offer_iri("b"));

    // rd tuple=: one tuple, as JSON-LD; the stamps survive although the task context does
    // not name `seq`.
    let one = ld(
        &f.kernel,
        TUPLES,
        &[("tuple", &offer_iri("a"))],
        &tasker(&taker),
    );
    let tuple = node(&one, &offer_iri("a"));
    assert_eq!(tuple["@type"], "Offer");
    assert_eq!(tuple["title"], "Task a");
    assert_eq!(
        tuple["repository"],
        "https://github.com/ikigai-rs/ikigai-layers"
    );
    assert_eq!(tuple["attributedTo"], plasma);
    assert_eq!(tuple["seq"], "1");

    // The take answers the tuple, as JSON-LD.
    let taken = json(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", &offer_iri("a")), ("as", JSON_LD)],
        &tasker(&taker),
        JSON_LD,
    );
    assert_eq!(node(&taken, &offer_iri("a"))["brief"], "Do a.");

    // A taker whose reply was lost reads the take back, in JSON: the receipt, holding the
    // tuple as taken.
    let receipt = ld(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("a"))],
        &tasker(&taker),
    );
    let nodes = graph(&receipt);
    assert_eq!(nodes.len(), 2, "{receipt}");
    let activity = nodes
        .iter()
        .find(|n| n["@type"] == "Activity")
        .unwrap_or_else(|| panic!("no Activity in {receipt}"));
    assert_eq!(activity["used"], offer_iri("a"));
    assert_eq!(activity["associatedWith"], taker);
    assert_eq!(activity["seq"], "3");
    assert!(activity["endedAt"]
        .as_str()
        .unwrap()
        .starts_with("2026-10-10T"));
    assert_eq!(node(&receipt, &offer_iri("a"))["title"], "Task a");
    let receipt_iri = activity["@id"].as_str().unwrap().to_string();

    // The same receipt by its own name.
    let id = receipt_iri.rsplit(':').next().unwrap();
    let by_name = ld(
        &f.kernel,
        &format!("urn:layer:tasks-bobby:receipt:{id}"),
        &[],
        &tasker(&taker),
    );
    assert_eq!(by_name, receipt);

    // The receipts as a list.
    let receipts = json(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("state", "taken"), ("as", JSON)],
        &tasker(&taker),
        JSON,
    );
    assert_eq!(receipts["layer"], "tasks-bobby");
    let listed = receipts["receipts"].as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["seq"], 3);
    assert_eq!(listed[0]["subject"], offer_iri("a"));
    assert_eq!(listed[0]["taker"], taker);
    assert_eq!(listed[0]["receipt"], receipt_iri);
    assert!(listed[0]["time"]
        .as_str()
        .unwrap()
        .starts_with("2026-10-10T"));

    // The pending tuples as a graph: the one left.
    let left = ld(&f.kernel, TUPLES, &[], &tasker(&taker));
    assert_eq!(graph(&left).len(), 1);
    assert_eq!(node(&left, &offer_iri("b"))["seq"], "2");

    // A second take is the Conflict it always was, whatever the face.
    match issue(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", &offer_iri("a")), ("as", JSON_LD)],
        &tasker(&plasma),
    ) {
        Err(Error::Conflict(m)) => assert!(m.contains(&taker), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn one_tuple_is_a_graph_never_a_list() {
    let f = fixture();
    out(&f.kernel, &gonk(), &offer("a")).unwrap();
    match issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("tuple", &offer_iri("a")), ("as", JSON)],
        &tasker(&gonk()),
    ) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "as");
            assert!(detail.contains(JSON_LD), "{detail}");
        }
        other => panic!("{other:?}"),
    }
}

// ---- declared, so every face sees it ------------------------------------------------------

#[test]
fn every_json_face_is_declared() {
    let f = fixture();
    let mut faces = std::collections::BTreeMap::new();
    for entry in f.kernel.entries().expect("an enumerable kernel") {
        let Some(description) = f.kernel.describe_pattern(&entry.pattern) else {
            continue;
        };
        for action in description.action_specs() {
            let offered: Vec<String> = action
                .inputs
                .iter()
                .find(|i| i.name == "as")
                .map(|i| i.one_of.clone())
                .unwrap_or_default();
            faces.insert(format!("{}:{:?}", description.id, action.verb), offered);
        }
    }
    let has = |key: &str, face: &str| {
        assert!(
            faces.get(key).is_some_and(|f| f.iter().any(|x| x == face)),
            "{key} does not declare {face}: {:?}",
            faces.get(key)
        )
    };
    for action in ["entries", "entry", "graph", "latest"] {
        has(&format!("layer-talk-{action}:Source"), JSON_LD);
    }
    has("layer-talk-say:Sink", JSON);
    has("layer-tasks-bobby-tuples:Source", JSON);
    has("layer-tasks-bobby-tuples:Source", JSON_LD);
    has("layer-tasks-bobby-tuples:Sink", JSON);
    has("layer-tasks-bobby-tuples:Delete", JSON_LD);
    has("layer-tasks-bobby-receipt:Source", JSON_LD);
}
