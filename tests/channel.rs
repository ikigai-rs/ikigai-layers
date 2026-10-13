//! The channel model: `say` names, stamps, admits and appends; the reads answer what was
//! said; nothing is ever changed.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use common::*;
use ikigai_core::{Capability, Error, Verb};
use ikigai_layers::id;

fn bobby() -> String {
    participant("bobby")
}

fn content_refusal(result: Result<String, Error>) -> String {
    match result {
        Err(Error::InvalidArgument { name, detail }) if name == "content" => detail,
        other => panic!("expected a refused content, got {other:?}"),
    }
}

#[test]
fn say_names_stamps_and_stores_one_entry() {
    let f = fixture();
    let iri = say(&f.kernel, "talk", &bobby(), &message("hello")).unwrap();
    let id = iri
        .strip_prefix("urn:layer:talk:entry:")
        .expect("an entry IRI");
    assert_eq!(id::seq_of(id), Some(1));

    let entry = source(&f.kernel, &iri, &[], &member(&bobby()));
    let t = triples(&entry);
    let has = |p: &str, o: &str| {
        t.iter().any(|t| {
            t.subject.to_string() == format!("<{iri}>")
                && t.predicate.as_str() == p
                && t.object.to_string() == o
        })
    };
    assert!(
        has(
            "http://www.w3.org/ns/prov#wasAttributedTo",
            &format!("<{}>", bobby())
        ),
        "{entry}"
    );
    // The kernel clock's reading, whatever it was: the same one the feed logs.
    let created = t
        .iter()
        .find(|t| t.predicate.as_str() == "http://purl.org/dc/terms/created")
        .map(|t| t.object.to_string())
        .expect("a creation time");
    let time = created
        .strip_prefix('"')
        .and_then(|c| c.strip_suffix("\"^^<http://www.w3.org/2001/XMLSchema#dateTime>"))
        .expect("an xsd:dateTime")
        .to_string();
    assert!(
        time.starts_with("2026-10-10T00:00:0") && time.ends_with(".000Z"),
        "{time}"
    );
    assert!(
        has(
            &format!("{IK}seq"),
            "\"1\"^^<http://www.w3.org/2001/XMLSchema#integer>"
        ),
        "{entry}"
    );
    assert!(has(&format!("{IK}body"), "\"hello\""), "{entry}");
    // ⚠ Never ik:layer (lowercase): it would type the entry an ik:Consultation.
    assert!(
        !entry.contains("ik:layer ") && !entry.contains("#layer>"),
        "{entry}"
    );

    // On disk, once, in the author's graph.
    let file = f
        .state
        .path()
        .join("talk/graphs/bobby")
        .join(format!("{id}.ttl"));
    assert!(file.exists(), "{}", file.display());
    // And in the feed.
    let feed = source(
        &f.kernel,
        "urn:layer:talk:feed",
        &[("after", "0")],
        &member(&bobby()),
    );
    assert_eq!(feed, format!("1 {time} say {iri} {}\n", bobby()));
}

#[test]
fn json_ld_says_the_same_thing() {
    let f = fixture();
    let jsonld = format!(
        r#"{{ "@context": {{ "ik": "{IK}" }}, "@id": "urn:layer:new", "@type": "ik:Message", "ik:body": "hi from JSON-LD" }}"#
    );
    let iri = say(&f.kernel, "talk", &bobby(), &jsonld).unwrap();
    let entry = source(&f.kernel, &iri, &[], &member(&bobby()));
    assert!(entry.contains("hi from JSON-LD"), "{entry}");
    // A blank-node subject is the same as <urn:layer:new>.
    let blank = format!(
        r#"{{ "@context": {{ "ik": "{IK}" }}, "@type": "ik:Message", "ik:body": "anon" }}"#
    );
    assert!(say(&f.kernel, "talk", &bobby(), &blank).is_ok());
}

#[test]
fn say_needs_a_principal_the_participants_graph_names() {
    let f = fixture();
    let root = issue(
        &f.kernel,
        Verb::Sink,
        "urn:layer:talk:say",
        &[("content", &message("x"))],
        &Capability::root(),
    );
    assert!(
        matches!(root, Err(Error::Denied(ref m)) if m.contains("principal")),
        "{root:?}"
    );
    let anonymous = issue(
        &f.kernel,
        Verb::Sink,
        "urn:layer:talk:say",
        &[("content", &message("x"))],
        &Capability::scoped(["urn:cap:layer:say:talk"]),
    );
    assert!(matches!(anonymous, Err(Error::Denied(_))), "{anonymous:?}");
    let stranger = say(&f.kernel, "talk", &participant("mallory"), &message("x"));
    assert!(
        matches!(stranger, Err(Error::Denied(ref m)) if m.contains("not a participant")),
        "{stranger:?}"
    );
}

#[test]
fn stamps_are_refused_if_sent() {
    let f = fixture();
    for (prefix, predicate, object) in [
        (
            "prov: <http://www.w3.org/ns/prov#>",
            "prov:wasAttributedTo",
            "<urn:collab:participant:brian>",
        ),
        (
            "dcterms: <http://purl.org/dc/terms/>",
            "dcterms:created",
            "\"2020-01-01T00:00:00Z\"",
        ),
        ("ik2: <https://ikigai-rs.dev/ns#>", "ik2:seq", "7"),
    ] {
        let content = format!(
            "@prefix ik: <{IK}> .\n@prefix {prefix} .\n<urn:layer:new> a ik:Message ; ik:body \"x\" ; {predicate} {object} .\n"
        );
        let detail = content_refusal(say(&f.kernel, "talk", &bobby(), &content));
        assert!(detail.contains("stamped by the host"), "{detail}");
    }
}

#[test]
fn an_entry_is_exactly_one_subject() {
    let f = fixture();
    let cases = [
        // Two subjects.
        format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; ik:body \"a\" .\n_:b a ik:Message ; ik:body \"b\" .\n"),
        // A subject of the author's own choosing.
        format!("@prefix ik: <{IK}> .\n<urn:layer:talk:entry:mine> a ik:Message ; ik:body \"a\" .\n"),
        // A blank node as an object.
        format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; ik:body \"a\" ; ik:inReplyTo [ ik:body \"b\" ] .\n"),
        // Nothing at all.
        String::new(),
    ];
    for content in cases {
        content_refusal(say(&f.kernel, "talk", &bobby(), &content));
    }
}

#[test]
fn a_type_no_shape_targets_is_refused() {
    let f = fixture();
    // SHACL would answer `conforms` for a class nothing targets: the host refuses first.
    let other = format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Handback ; ik:body \"x\" .\n");
    let detail = content_refusal(say(&f.kernel, "talk", &bobby(), &other));
    assert!(detail.contains("admits only"), "{detail}");
    let untyped = format!("@prefix ik: <{IK}> .\n<urn:layer:new> ik:body \"x\" .\n");
    assert!(content_refusal(say(&f.kernel, "talk", &bobby(), &untyped)).contains("no rdf:type"));
}

#[test]
fn admission_reads_conforms_not_ok() {
    let f = fixture();
    // No body: the validator answers a report with `conforms: false`, not an error.
    let bodiless = format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message .\n");
    let detail = content_refusal(say(&f.kernel, "talk", &bobby(), &bodiless));
    assert!(detail.contains("does not conform"), "{detail}");
    // A predicate the CLOSED shape does not mention.
    let extra = format!(
        "@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; ik:body \"x\" ; <urn:example:secret> \"y\" .\n"
    );
    let detail = content_refusal(say(&f.kernel, "talk", &bobby(), &extra));
    assert!(detail.contains("does not conform"), "{detail}");
    // Nothing refused was stored or logged.
    let feed = source(
        &f.kernel,
        "urn:layer:talk:feed",
        &[("after", "0")],
        &member(&bobby()),
    );
    assert_eq!(feed, "");
}

/// Admission is the host's own call: a layer whose shapes the validator cannot compile would
/// admit nothing, so it stops the host when it opens, never at the first write.
#[test]
fn shapes_the_validator_cannot_compile_stop_the_host() {
    let config = Scratch::new("bad-shapes");
    write_config(&config);
    config.write(
        "layers/msg.shapes.ttl",
        &SHAPES.replace("sh:maxLength 4000", "sh:maxLength \"long\""),
    );
    let state = Scratch::new("bad-shapes-state");
    let refused = ikigai_layers::Host::open(config.path(), state.path());
    let Err(e) = refused else {
        panic!("shapes the validator cannot compile opened a host");
    };
    assert!(e.to_string().contains("cannot compile"), "{e}");
}

#[test]
fn the_entry_bound_is_the_layers() {
    let f = fixture();
    // talk-bobby is bounded at 2 KiB.
    let long = message(&"x".repeat(2100));
    assert!(content_refusal(say(&f.kernel, "talk-bobby", &bobby(), &long)).contains("bound"));
    assert!(say(&f.kernel, "talk", &bobby(), &long).is_ok());
}

#[test]
fn reads_filter_and_page() {
    let f = fixture();
    let brian = participant("brian");
    let a = say(&f.kernel, "talk", &bobby(), &message("one")).unwrap();
    let question =
        format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Question ; ik:body \"two?\" .\n");
    let b = say(&f.kernel, "talk", &brian, &question).unwrap();
    let c = say(&f.kernel, "talk", &bobby(), &message("three")).unwrap();
    let reader = member(&bobby());

    let all = source(&f.kernel, "urn:layer:talk:entries", &[], &reader);
    for iri in [&a, &b, &c] {
        assert!(all.contains(iri.as_str()), "{all}");
    }
    let after = source(
        &f.kernel,
        "urn:layer:talk:entries",
        &[("after", "1")],
        &reader,
    );
    assert!(!after.contains(a.as_str()) && after.contains(b.as_str()));
    let page = source(
        &f.kernel,
        "urn:layer:talk:entries",
        &[("limit", "1")],
        &reader,
    );
    assert!(page.contains(a.as_str()) && !page.contains(b.as_str()));
    let from = source(
        &f.kernel,
        "urn:layer:talk:entries",
        &[("from", &brian)],
        &reader,
    );
    assert!(from.contains(b.as_str()) && !from.contains(a.as_str()));
    let typed = source(
        &f.kernel,
        "urn:layer:talk:entries",
        &[("type", &format!("{IK}Question"))],
        &reader,
    );
    assert!(typed.contains(b.as_str()) && !typed.contains(c.as_str()));
    let graph = source(&f.kernel, "urn:layer:talk:graph:bobby", &[], &reader);
    assert!(
        graph.contains(a.as_str()) && graph.contains(c.as_str()) && !graph.contains(b.as_str())
    );

    let too_many = issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk:entries",
        &[("limit", "1001")],
        &reader,
    );
    assert!(matches!(too_many, Err(Error::InvalidArgument { ref name, .. }) if name == "limit"));
}

#[test]
fn a_cached_read_is_cut_by_the_next_say() {
    let f = fixture();
    let reader = member(&bobby());
    let first = say(&f.kernel, "talk", &bobby(), &message("one")).unwrap();
    let entries = request(Verb::Source, "urn:layer:talk:entries", &[]);
    let before = source(&f.kernel, "urn:layer:talk:entries", &[], &reader);
    assert!(
        f.kernel.is_cached(&entries, &reader),
        "entries are cacheable"
    );
    let second = say(&f.kernel, "talk", &bobby(), &message("two")).unwrap();
    assert!(!f.kernel.is_cached(&entries, &reader), "a say cuts them");
    let after = source(&f.kernel, "urn:layer:talk:entries", &[], &reader);
    assert!(before.contains(first.as_str()) && !before.contains(second.as_str()));
    assert!(after.contains(second.as_str()));
}

#[test]
fn latest_answers_the_newest_unsuperseded_and_a_cached_absence_ends() {
    let f = fixture();
    let reader = member(&bobby());
    let topic = "urn:example:topic";
    let about = |body: &str, extra: &str| {
        format!(
            "@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n<urn:layer:new> a ik:Message ; ik:body \"{body}\" ; dcterms:subject <{topic}> {extra}.\n"
        )
    };
    let latest = || {
        issue(
            &f.kernel,
            Verb::Source,
            "urn:layer:talk:latest",
            &[("about", topic)],
            &reader,
        )
    };
    assert!(matches!(latest(), Err(Error::NotFound(_))));
    let first = say(&f.kernel, "talk", &bobby(), &about("v1", "")).unwrap();
    // The absence did not outlive the entry that ended it.
    assert!(text(&latest().unwrap()).contains(first.as_str()));
    let second = say(&f.kernel, "talk", &bobby(), &about("v2", "")).unwrap();
    assert!(text(&latest().unwrap()).contains(second.as_str()));
    // A correction supersedes the newest; latest is the correction.
    let fix = say(
        &f.kernel,
        "talk",
        &bobby(),
        &about("v2, corrected", &format!("; ik:supersedes <{second}> ")),
    )
    .unwrap();
    assert!(text(&latest().unwrap()).contains(fix.as_str()));
}

#[test]
fn an_entry_supersedes_only_its_own_authors() {
    let f = fixture();
    let mine = say(&f.kernel, "talk", &bobby(), &message("mine")).unwrap();
    let theirs = format!(
        "@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; ik:body \"no\" ; ik:supersedes <{mine}> .\n"
    );
    let refused = say(&f.kernel, "talk", &participant("brian"), &theirs);
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    let nowhere = format!(
        "@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; ik:body \"no\" ; ik:supersedes <urn:layer:talk:entry:0000000000000000000000009> .\n"
    );
    content_refusal(say(&f.kernel, "talk", &bobby(), &nowhere));
}

#[test]
fn an_entry_read_is_exact_and_a_missing_one_is_not_found() {
    let f = fixture();
    let reader = member(&bobby());
    let missing = issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk:entry:00000000000000000000001",
        &[],
        &reader,
    );
    assert!(matches!(missing, Err(Error::NotFound(_))));
    // An entry of one layer is not readable through another's name.
    let iri = say(&f.kernel, "talk", &bobby(), &message("x")).unwrap();
    let id = iri.rsplit(':').next().unwrap();
    let elsewhere = issue(
        &f.kernel,
        Verb::Source,
        &format!("urn:layer:notes:entry:{id}"),
        &[],
        &reader,
    );
    assert!(matches!(elsewhere, Err(Error::NotFound(_))));
}

#[test]
fn a_grant_on_one_layer_reaches_no_other() {
    let f = fixture();
    say(&f.kernel, "talk-bobby", &bobby(), &message("private")).unwrap();
    // Holds read on `talk` and nothing on `talk-bobby`, whose name begins the same way.
    let talk_only = cap(&participant("chris"), &["urn:cap:layer:read:talk"]);
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk-bobby:entries",
        &[],
        &talk_only,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    // A literal held `*` is a string, not a family: it allows only itself.
    let star = cap(&participant("chris"), &["urn:cap:layer:read:*"]);
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk:entries",
        &[],
        &star,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
}

#[test]
fn a_restart_keeps_everything_and_continues_the_sequence() {
    let mut f = fixture();
    let a = say(&f.kernel, "talk", &bobby(), &message("before")).unwrap();
    f.reopen();
    let b = say(&f.kernel, "talk", &bobby(), &message("after")).unwrap();
    assert_eq!(id::seq_of(b.rsplit(':').next().unwrap()), Some(2));
    let all = source(&f.kernel, "urn:layer:talk:entries", &[], &member(&bobby()));
    assert!(all.contains(a.as_str()) && all.contains(b.as_str()));
}

#[test]
fn a_feed_that_lost_an_event_is_repaired_on_open() {
    let mut f = fixture();
    say(&f.kernel, "talk", &bobby(), &message("one")).unwrap();
    let two = say(&f.kernel, "talk", &bobby(), &message("two")).unwrap();
    // A crash between storing the entry and logging it: the last feed line is gone, and
    // a torn half-line was left behind.
    let log = f.state.path().join("talk/feed.log");
    let text = std::fs::read_to_string(&log).unwrap();
    let first_line = text.lines().next().unwrap();
    std::fs::write(&log, format!("{first_line}\n2\t2026")).unwrap();
    f.reopen();
    let feed = source(
        &f.kernel,
        "urn:layer:talk:feed",
        &[("after", "1")],
        &member(&bobby()),
    );
    assert!(
        feed.starts_with("2 ") && feed.contains(two.as_str()),
        "{feed}"
    );
    let three = say(&f.kernel, "talk", &bobby(), &message("three")).unwrap();
    assert_eq!(id::seq_of(three.rsplit(':').next().unwrap()), Some(3));
}

#[test]
fn the_definition_shapes_and_context_are_served() {
    let f = fixture();
    let reader = member(&bobby());
    let def = source(&f.kernel, "urn:layer:talk", &[], &reader);
    let t = triples(&def);
    let has = |p: &str, o: &str| {
        t.iter()
            .any(|t| t.predicate.as_str() == p && t.object.to_string() == o)
    };
    assert!(
        has(
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
            &format!("<{IK}Layer>")
        ),
        "{def}"
    );
    assert!(has(&format!("{IK}layerModel"), "\"channel\""), "{def}");
    assert!(has(
        "http://www.w3.org/2000/01/rdf-schema#seeAlso",
        "<urn:layer:talk:shapes>"
    ));
    assert!(has(
        "http://purl.org/dc/terms/isPartOf",
        "<urn:collab:chris>"
    ));
    let shapes = source(&f.kernel, "urn:layer:talk:shapes", &[], &reader);
    assert!(
        triples(&shapes)
            .iter()
            .all(|t| !ikigai_layers::rdf::has_blank_node(t)),
        "the served shapes are skolemized: {shapes}"
    );
    assert!(shapes.contains("urn:layer:talk:shapes:n0"), "{shapes}");
    let context = source(&f.kernel, "urn:layer:talk:context", &[], &reader);
    assert!(context.contains("\"body\""));
}
