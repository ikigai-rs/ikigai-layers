//! Feed layers (ledger #3284): publishers only, hidden membership, one shared copy, a SKOS
//! scheme, the graph, filtered entries, the Atom face and the JSON faces.

#![cfg(feature = "host")]

mod common;

use common::*;
use ikigai_core::{Capability, Error, Verb};
use ikigai_layers::Host;

const LAYER: &str = "releases";
const ENTRIES: &str = "urn:layer:releases:entries";
const GRAPH: &str = "urn:layer:releases:graph";
const SCHEME: &str = "urn:layer:releases:scheme";
const DEFINITION: &str = "urn:layer:releases";
const RELEASE: &str = "urn:example:concept:release";
const PATCH: &str = "urn:example:concept:patch";
const FEATURE: &str = "urn:example:concept:feature";

const FEED_SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<urn:example:shape:announcement> a sh:NodeShape ;
  sh:targetClass ik:Message, <urn:example:Special> ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 2 ; sh:datatype xsd:string ] ,
              [ sh:path ik:body ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;

const FEED_CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#",
  "dcterms": "http://purl.org/dc/terms/",
  "title": "dcterms:title",
  "body": "ik:body",
  "subject": { "@id": "dcterms:subject", "@type": "@id" }
} }"#;

const SCHEME_TTL: &str = r#"@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
<urn:example:scheme:releases> a skos:ConceptScheme ; rdfs:label "Release types" .
<urn:example:concept:release> a skos:Concept ; skos:prefLabel "Release" ; skos:notation "1" .
<urn:example:concept:patch> a skos:Concept ; skos:prefLabel "Patch" ; skos:notation "1.1" ;
  skos:broader <urn:example:concept:release> .
<urn:example:concept:feature> a skos:Concept ; skos:prefLabel "Feature" ; skos:notation "2" .
"#;

/// A host with the `releases` feed, read by Bobby and Chris and published by Brian.
fn feed_fixture() -> Fixture {
    feed_fixture_with_scheme(SCHEME_TTL)
}

fn feed_fixture_with_scheme(scheme: &str) -> Fixture {
    feed_fixture_with_scheme_and_options(scheme,
        "model = \"channel\"\nlabel = \"Releases\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nshared = [\"audience\"]\nfeed = true\npublic = true\npublishers = [\"brian\"]\nscheme = \"releases.scheme.ttl\"\n",
        "[[mount]]\nlayer = \"releases\"\nbrian = [\"read\", \"feed\", \"say\"]\nbobby = [\"read\", \"feed\"]\nchris = [\"read\"]\n",
    )
    .expect("the feed fixture opens")
}

fn feed_fixture_with(definition: &str, audience: &str) -> Result<Fixture, String> {
    feed_fixture_with_scheme_and_options(SCHEME_TTL, definition, audience)
}

fn feed_fixture_with_scheme_and_options(
    scheme: &str,
    definition: &str,
    audience: &str,
) -> Result<Fixture, String> {
    let config = Scratch::new("config");
    write_config(&config);
    config.write("layers/feed.shapes.ttl", FEED_SHAPES);
    config.write("layers/feed.context.jsonld", FEED_CONTEXT);
    config.write("layers/releases.scheme.ttl", scheme);
    config.write("layers/releases.toml", definition);
    config.write("collabs/audience.toml", audience);
    let state = Scratch::new("state");
    let host =
        Host::open(config.path(), state.path(), &key(config.path())).map_err(|e| e.to_string())?;
    let kernel = kernel_over(&host);
    Ok(Fixture {
        config,
        state,
        host,
        kernel,
    })
}

fn publisher() -> Capability {
    cap(
        &participant("brian"),
        &["urn:cap:layer:read:releases", "urn:cap:layer:say:releases"],
    )
}

fn publish(f: &Fixture, title: &str, concepts: &[&str]) -> String {
    let subjects: String = concepts
        .iter()
        .map(|c| format!(" dcterms:subject <{c}> ;"))
        .collect();
    let content = format!(
        "@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
         <urn:layer:new> a ik:Message ;{subjects} dcterms:title \"{title}\" ; ik:body \"About {title}.\" .\n"
    );
    text(
        &issue(
            &f.kernel,
            Verb::Sink,
            "urn:layer:releases:say",
            &[("content", &content)],
            &publisher(),
        )
        .expect("a publisher says"),
    )
}

fn reader(name: &str) -> Capability {
    // Different grants: what a reader holds beyond the read token differs.
    if name == "bobby" {
        cap(
            &participant(name),
            &[
                "urn:cap:layer:read:releases",
                "urn:cap:layer:feed:releases",
                "urn:cap:layer:read:talk",
            ],
        )
    } else {
        cap(&participant(name), &["urn:cap:layer:read:releases"])
    }
}

#[test]
fn a_non_publisher_with_a_say_grant_is_refused_naming_the_feed() {
    let f = feed_fixture();
    let bobby = cap(
        &participant("bobby"),
        &["urn:cap:layer:say:releases", "urn:cap:layer:read:releases"],
    );
    let content = format!(
        "@prefix dcterms: <http://purl.org/dc/terms/> .\n@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; dcterms:title \"nope\" .\n"
    );
    match issue(
        &f.kernel,
        Verb::Sink,
        "urn:layer:releases:say",
        &[("content", &content)],
        &bobby,
    ) {
        Err(Error::Denied(why)) => assert!(
            why.contains("releases") && why.contains("feed"),
            "the refusal names the layer as a feed: {why}"
        ),
        other => panic!("expected Denied, got {other:?}"),
    }
}

#[test]
fn the_grant_generator_refuses_say_on_a_feed_to_a_non_publisher_naming_both() {
    let err = match feed_fixture_with(
        "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\npublishers = [\"brian\"]\nscheme = \"releases.scheme.ttl\"\n",
        "[[mount]]\nlayer = \"releases\"\nbobby = [\"read\", \"say\"]\n",
    ) {
        Err(e) => e,
        Ok(_) => panic!("a manifest granting say on a feed to a non-publisher must be refused"),
    };
    assert!(
        err.contains("releases") && err.contains("urn:collab:participant:bobby"),
        "the refusal names the feed and the participant: {err}"
    );
}

#[test]
fn a_feed_definition_is_typed_feed_and_names_no_collaboration() {
    let f = feed_fixture();
    let body = source(&f.kernel, DEFINITION, &[], &reader("bobby"));
    assert!(body.contains("https://ikigai-rs.dev/ns#Feed") || body.contains("ik:Feed"));
    assert!(body.contains("ik:Layer") || body.contains("ns#Layer"));
    assert!(
        !body.contains("isPartOf") && !body.contains("urn:collab:audience"),
        "a feed definition names no collaboration: {body}"
    );
}

#[test]
fn no_collaboration_answer_names_a_feeds_readers() {
    let f = feed_fixture();
    let bobby = cap(
        &participant("bobby"),
        &[
            "urn:cap:layer:read:releases",
            "urn:cap:collab:read:audience",
        ],
    );
    let brian_named = "urn:collab:participant:brian";
    for iri in ["urn:collab:audience", "urn:collab:audience:participants"] {
        let body = source(&f.kernel, iri, &[], &bobby);
        assert!(body.contains(brian_named), "the publisher is named: {body}");
        assert!(
            !body.contains("urn:collab:participant:bobby")
                && !body.contains("urn:collab:participant:chris"),
            "{iri} names no reader: {body}"
        );
    }
    let json = source(
        &f.kernel,
        "urn:collab:audience",
        &[("as", "application/json")],
        &bobby,
    );
    assert!(!json.contains("urn:collab:participant:bobby"));
}

#[test]
fn a_plain_channels_definition_still_lists_its_collaborations() {
    let f = feed_fixture();
    let talk = cap(&participant("bobby"), &["urn:cap:layer:read:talk"]);
    let body = source(&f.kernel, "urn:layer:talk", &[], &talk);
    assert!(
        body.contains("isPartOf") || body.contains("hasPart"),
        "{body}"
    );
    assert!(body.contains("urn:collab:bobby"), "{body}");
    assert!(!body.contains("Feed"), "{body}");
}

#[test]
fn two_readers_with_different_grants_share_one_computation_and_answer_byte_identically() {
    let f = feed_fixture();
    publish(&f, "Release 1.0", &[RELEASE]);
    let warmed = f.host.feed_computations(LAYER);

    let bobby = source(&f.kernel, ENTRIES, &[], &reader("bobby"));
    let chris = source(&f.kernel, ENTRIES, &[], &reader("chris"));
    assert_eq!(bobby, chris, "byte-identical answers for different grants");
    assert_eq!(
        f.host.feed_computations(LAYER),
        warmed,
        "a publish warmed the default page, so neither reader computed anything"
    );

    // A page nothing warmed: one miss, whoever comes first.
    let asked = &[("limit", "5")];
    let bobby = source(&f.kernel, ENTRIES, asked, &reader("bobby"));
    let after_first = f.host.feed_computations(LAYER);
    assert_eq!(after_first, warmed + 1, "one computation for the new page");
    let chris = source(&f.kernel, ENTRIES, asked, &reader("chris"));
    assert_eq!(bobby, chris);
    assert_eq!(
        f.host.feed_computations(LAYER),
        after_first,
        "and no second"
    );
}

#[test]
fn a_publish_warms_the_default_page_and_the_graph_and_cuts_the_old_copy() {
    let f = feed_fixture();
    let before = f.host.feed_computations(LAYER);
    let entry = publish(&f, "Release 1.0", &[RELEASE]);
    assert_eq!(
        f.host.feed_computations(LAYER),
        before + 2,
        "the default entries page and the graph are computed once at publish"
    );
    assert!(source(&f.kernel, GRAPH, &[], &reader("chris")).contains("Release 1.0"));
    assert_eq!(f.host.feed_computations(LAYER), before + 2, "already warm");
    let second = publish(&f, "Release 2.0", &[FEATURE]);
    let page = source(&f.kernel, ENTRIES, &[], &reader("bobby"));
    assert!(page.contains(entry.as_str()) && page.contains(second.as_str()));
}

#[test]
fn reading_a_feed_needs_the_callers_own_read_token() {
    let f = feed_fixture();
    publish(&f, "Release 1.0", &[RELEASE]);
    let outsider = cap(&participant("chris"), &["urn:cap:layer:read:talk"]);
    match issue(&f.kernel, Verb::Source, ENTRIES, &[], &outsider) {
        Err(Error::Denied(_) | Error::NotFound(_)) => {}
        other => panic!("a reader without the token is refused, got {other:?}"),
    }
}

#[test]
fn an_entry_subject_outside_the_scheme_is_refused_naming_it() {
    let f = feed_fixture();
    let content = format!(
        "@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
         <urn:layer:new> a ik:Message ; dcterms:title \"bad\" ; dcterms:subject <urn:example:concept:nope> .\n"
    );
    match issue(
        &f.kernel,
        Verb::Sink,
        "urn:layer:releases:say",
        &[("content", &content)],
        &publisher(),
    ) {
        Err(Error::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("urn:example:concept:nope"), "{detail}")
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn the_graph_is_the_ikigai_feed_form_unioned_with_the_scheme() {
    let f = feed_fixture();
    let entry = publish(&f, "Release 1.0", &[PATCH]);
    let body = source(&f.kernel, GRAPH, &[], &reader("bobby"));
    let graph = triples(&body);
    let has = |s: &str, p: &str, o_part: &str| {
        graph.iter().any(|t| {
            t.subject.to_string().contains(s)
                && t.predicate.as_str() == p
                && t.object.to_string().contains(o_part)
        })
    };
    assert!(has(
        &entry,
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "http://purl.org/rss/1.0/item"
    ));
    assert!(has(&entry, "http://purl.org/rss/1.0/title", "Release 1.0"));
    assert!(has(
        &entry,
        "http://purl.org/rss/1.0/modules/content/encoded",
        "About Release 1.0"
    ));
    assert!(has(
        &entry,
        "http://purl.org/dc/terms/issued",
        "XMLSchema#dateTime"
    ));
    assert!(has(
        &entry,
        "http://purl.org/dc/elements/1.1/creator",
        "urn:collab:participant:brian"
    ));
    assert!(has(&entry, "http://purl.org/dc/terms/subject", PATCH));
    assert!(has(
        &entry,
        "http://purl.org/dc/terms/isPartOf",
        "urn:layer:releases"
    ));
    assert!(has(
        PATCH,
        "http://www.w3.org/2004/02/skos/core#broader",
        RELEASE
    ));
}

#[test]
fn concept_filter_reaches_narrower_concepts_and_leaves_others_out() {
    let f = feed_fixture();
    let patch = publish(&f, "Patch 1.0.1", &[PATCH]);
    let feature = publish(&f, "Feature x", &[FEATURE]);
    let body = source(
        &f.kernel,
        ENTRIES,
        &[("concept", RELEASE)],
        &reader("bobby"),
    );
    assert!(
        body.contains(&patch),
        "a narrower concept is reached: {body}"
    );
    assert!(!body.contains(&feature), "and one outside is not: {body}");
    let both = source(
        &f.kernel,
        ENTRIES,
        &[("concept", &format!("{FEATURE},{RELEASE}"))],
        &reader("chris"),
    );
    assert!(both.contains(&patch) && both.contains(&feature), "{both}");
}

#[test]
fn an_unknown_concept_is_refused_naming_it() {
    let f = feed_fixture();
    match issue(
        &f.kernel,
        Verb::Source,
        ENTRIES,
        &[("concept", "urn:example:concept:nope")],
        &reader("bobby"),
    ) {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "concept");
            assert!(detail.contains("urn:example:concept:nope"), "{detail}");
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

#[test]
fn the_atom_face_carries_every_element_atom_requires() {
    let f = feed_fixture();
    let entry = publish(&f, "Release 1.0", &[RELEASE]);
    let atom = source(
        &f.kernel,
        ENTRIES,
        &[("as", "application/atom+xml")],
        &reader("bobby"),
    );
    assert!(atom.contains("http://www.w3.org/2005/Atom"), "{atom}");
    assert!(atom.contains("<feed"), "{atom}");
    assert!(atom.contains("<id>urn:layer:releases</id>"), "{atom}");
    assert!(atom.contains("<title>Releases</title>"), "{atom}");
    assert!(atom.contains("<updated>"), "{atom}");
    assert!(atom.contains("<entry>"), "{atom}");
    assert!(atom.contains(&format!("<id>{entry}</id>")), "{atom}");
    assert!(atom.contains("<title>Release 1.0</title>"), "{atom}");
    assert!(atom.contains("<published>"), "{atom}");
    assert!(
        atom.contains("<author><name>urn:collab:participant:brian</name></author>"),
        "{atom}"
    );
    assert!(atom.contains("About Release 1.0."), "{atom}");
    let empty = source(
        &f.kernel,
        ENTRIES,
        &[("as", "application/atom+xml"), ("concept", FEATURE)],
        &reader("chris"),
    );
    assert!(empty.contains("<title>Releases</title>"), "{empty}");
    assert!(!empty.contains("<entry>"), "{empty}");
}

#[test]
fn the_definition_json_is_exactly_its_shape() {
    let f = feed_fixture();
    let body = source(
        &f.kernel,
        DEFINITION,
        &[("as", "application/json")],
        &reader("bobby"),
    );
    let got: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        got,
        serde_json::json!({
            "layer": "releases", "label": "Releases", "model": "channel",
            "types": ["Layer", "Feed"], "feed": true, "public": true,
        })
    );
    let talk = cap(&participant("bobby"), &["urn:cap:layer:read:talk"]);
    let plain = source(
        &f.kernel,
        "urn:layer:talk",
        &[("as", "application/json")],
        &talk,
    );
    let got: serde_json::Value = serde_json::from_str(&plain).unwrap();
    assert_eq!(
        got,
        serde_json::json!({
            "layer": "talk", "label": "Talk", "model": "channel",
            "types": ["Layer"], "feed": false, "public": false,
        })
    );
}

#[test]
fn the_scheme_json_is_exactly_its_shape_in_notation_order() {
    let f = feed_fixture();
    let body = source(
        &f.kernel,
        SCHEME,
        &[("as", "application/json")],
        &reader("bobby"),
    );
    let got: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        got,
        serde_json::json!({
            "scheme": "urn:example:scheme:releases",
            "label": "Release types",
            "concepts": [
                {"@id": RELEASE, "prefLabel": "Release", "notation": "1", "broader": []},
                {"@id": PATCH, "prefLabel": "Patch", "notation": "1.1", "broader": [RELEASE]},
                {"@id": FEATURE, "prefLabel": "Feature", "notation": "2", "broader": []},
            ],
        })
    );
    let turtle = source(&f.kernel, SCHEME, &[], &reader("bobby"));
    assert!(turtle.contains("ConceptScheme"), "{turtle}");
}

#[test]
fn feed_options_are_refused_where_they_mean_nothing() {
    let cases = [
        (
            "model = \"tuplespace\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\n",
            "`feed` is a channel option",
        ),
        (
            "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\npublishers = [\"brian\"]\n",
            "`publishers` is a feed option",
        ),
        (
            "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\nscheme = \"releases.scheme.ttl\"\n",
            "needs `publishers",
        ),
        (
            "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\npublishers = [\"brian\"]\n",
            "needs `scheme",
        ),
        (
            "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\npublishers = [\"nobody\"]\nscheme = \"releases.scheme.ttl\"\n",
            "does not name",
        ),
    ];
    for (definition, expected) in cases {
        let err = feed_fixture_with(definition, "label = \"empty\"\n")
            .err()
            .unwrap_or_else(|| panic!("should refuse: {definition}"));
        assert!(err.contains(expected), "{expected} in {err}");
    }
}

#[test]
fn an_unmatched_requested_concept_does_not_consume_a_page_slot() {
    let f = feed_fixture();
    let entry = publish(&f, "Release", &[RELEASE]);
    let concepts = format!("{FEATURE},{RELEASE}");
    for face in ["text/turtle", "application/atom+xml"] {
        let page = source(
            &f.kernel,
            ENTRIES,
            &[("concept", &concepts), ("limit", "1"), ("as", face)],
            &reader("bobby"),
        );
        assert!(page.contains(&entry), "{face}: {page}");
    }
}

#[test]
fn concept_and_type_filters_apply_before_the_page_limit() {
    let f = feed_fixture();
    publish(&f, "First", &[RELEASE]);
    let content = format!("@prefix ik: <{IK}> . @prefix dcterms: <http://purl.org/dc/terms/> . <urn:layer:new> a ik:Message, <urn:example:Special> ; dcterms:title \"Second\" ; dcterms:subject <{RELEASE}> .");
    let entry = text(
        &issue(
            &f.kernel,
            Verb::Sink,
            "urn:layer:releases:say",
            &[("content", &content)],
            &publisher(),
        )
        .unwrap(),
    );
    let page = source(
        &f.kernel,
        ENTRIES,
        &[
            ("concept", RELEASE),
            ("type", "urn:example:Special"),
            ("limit", "1"),
        ],
        &reader("bobby"),
    );
    assert!(page.contains(&entry), "{page}");
}

#[test]
fn concept_filter_follows_mixed_hierarchy_directions_transitively() {
    let scheme = SCHEME_TTL.replace(
        "skos:broader <urn:example:concept:release>",
        "skos:narrower <urn:example:concept:feature>",
    ) + "\n<urn:example:concept:release> skos:narrower <urn:example:concept:patch> .";
    let f = feed_fixture_with_scheme(&scheme);
    let entry = publish(&f, "Deep descendant", &[FEATURE]);
    let page = source(
        &f.kernel,
        ENTRIES,
        &[("concept", RELEASE)],
        &reader("bobby"),
    );
    assert!(page.contains(&entry), "{page}");
}

#[test]
fn the_graph_jsonld_preserves_scheme_relationships() {
    let scheme = SCHEME_TTL.to_string() + "\n<urn:example:concept:release> skos:narrower <urn:example:concept:patch> ; skos:exactMatch <urn:other:release> ; skos:closeMatch <urn:other:similar> ; <urn:example:annotation> \"An annotation\"@en .";
    let f = feed_fixture_with_scheme(&scheme);
    publish(&f, "Release", &[RELEASE]);
    publish(&f, "Patch", &[PATCH]);
    let turtle = source(&f.kernel, GRAPH, &[], &reader("bobby"));
    let ld = source(
        &f.kernel,
        GRAPH,
        &[("as", "application/ld+json")],
        &reader("bobby"),
    );
    use oxrdfio::{RdfFormat, RdfParser};
    let expanded: std::collections::HashSet<_> = RdfParser::from_format(RdfFormat::JsonLd {
        profile: Default::default(),
    })
    .for_reader(ld.as_bytes())
    .map(|q| {
        let q = q.unwrap();
        oxrdf::Triple::new(q.subject, q.predicate, q.object)
    })
    .collect();
    let original: std::collections::HashSet<_> = triples(&turtle).into_iter().collect();
    assert_eq!(expanded, original);
}

#[test]
fn a_concept_iri_is_not_rewritten_as_a_query_placeholder() {
    let concept = "urn:example:concept:__AFTER____LIMIT__";
    let scheme = SCHEME_TTL.replace(RELEASE, concept);
    let f = feed_fixture_with_scheme(&scheme);
    let entry = publish(&f, "Placeholder in IRI", &[concept]);
    let page = source(
        &f.kernel,
        ENTRIES,
        &[("concept", concept)],
        &reader("bobby"),
    );
    assert!(page.contains(&entry), "{page}");
}

#[test]
fn hierarchy_cycles_terminate_and_mappings_and_labels_do_not_expand_the_filter() {
    let scheme = SCHEME_TTL.to_string()
        + r#"
<urn:example:concept:release> skos:broader <urn:example:concept:patch> ;
    skos:exactMatch <urn:example:concept:feature> ; skos:closeMatch <urn:other:release> .
<urn:example:concept:feature> skos:prefLabel "Release" .
"#;
    let f = feed_fixture_with_scheme(&scheme);
    let patch = publish(&f, "In cycle", &[PATCH]);
    let unrelated = publish(&f, "Same label and mapping", &[FEATURE]);
    let page = source(
        &f.kernel,
        ENTRIES,
        &[("concept", RELEASE)],
        &reader("bobby"),
    );
    assert!(page.contains(&patch), "{page}");
    assert!(!page.contains(&unrelated), "{page}");
    assert!(matches!(issue(&f.kernel, Verb::Source, ENTRIES,
        &[("concept", "urn:other:release")], &reader("bobby")),
        Err(Error::InvalidArgument { name, .. }) if name == "concept"));
}

#[test]
fn entry_pages_stay_in_sequence_when_a_publish_arrives_between_requests() {
    let f = feed_fixture();
    let first = publish(&f, "First", &[PATCH, RELEASE]);
    let second = publish(&f, "Second", &[RELEASE]);
    let third = publish(&f, "Third", &[PATCH]);
    let concepts = format!("{PATCH},{RELEASE},{FEATURE}");
    for face in ["text/turtle", "application/atom+xml"] {
        let page = source(
            &f.kernel,
            ENTRIES,
            &[("concept", &concepts), ("limit", "2"), ("as", face)],
            &reader("bobby"),
        );
        assert!(
            page.find(&first).unwrap() < page.find(&second).unwrap(),
            "{page}"
        );
        assert!(!page.contains(&third), "{page}");
        if face == "application/atom+xml" {
            assert_eq!(page.matches("<entry>").count(), 2, "{page}");
        }
    }
    let fourth = publish(&f, "Fourth", &[RELEASE]);
    for face in ["text/turtle", "application/atom+xml"] {
        let page = source(
            &f.kernel,
            ENTRIES,
            &[
                ("concept", &concepts),
                ("after", "2"),
                ("limit", "2"),
                ("as", face),
            ],
            &reader("bobby"),
        );
        assert!(
            page.find(&third).unwrap() < page.find(&fourth).unwrap(),
            "{page}"
        );
        assert!(!page.contains(&first) && !page.contains(&second), "{page}");
        let empty = source(
            &f.kernel,
            ENTRIES,
            &[("concept", &concepts), ("after", "4"), ("as", face)],
            &reader("bobby"),
        );
        assert!(!empty.contains(&fourth), "{empty}");
        if face == "application/atom+xml" {
            assert_eq!(page.matches("<entry>").count(), 2, "{page}");
            assert!(empty.contains("<id>urn:layer:releases</id>"), "{empty}");
        }
    }
}

#[test]
fn empty_graph_and_jsonld_faces_describe_the_same_feed_and_scheme() {
    let f = feed_fixture();
    let turtle = source(&f.kernel, GRAPH, &[], &reader("bobby"));
    let ld = source(
        &f.kernel,
        GRAPH,
        &[("as", "application/ld+json")],
        &reader("bobby"),
    );
    let parsed: std::collections::HashSet<_> =
        oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::JsonLd {
            profile: Default::default(),
        })
        .for_reader(ld.as_bytes())
        .map(|q| {
            let q = q.unwrap();
            oxrdf::Triple::new(q.subject, q.predicate, q.object)
        })
        .collect();
    assert_eq!(parsed, triples(&turtle).into_iter().collect());
    assert!(turtle.contains("urn:layer:releases"));
}

#[test]
fn multiple_titles_do_not_repeat_an_entry_or_consume_its_neighbors_page_slot() {
    let f = feed_fixture();
    let content = format!("@prefix ik: <{IK}> . @prefix dcterms: <http://purl.org/dc/terms/> . <urn:layer:new> a ik:Message ; dcterms:title \"Alpha\", \"Beta\" ; dcterms:subject <{RELEASE}> .");
    let first = text(
        &issue(
            &f.kernel,
            Verb::Sink,
            "urn:layer:releases:say",
            &[("content", &content)],
            &publisher(),
        )
        .unwrap(),
    );
    let second = publish(&f, "Second", &[RELEASE]);
    for face in ["text/turtle", "application/atom+xml"] {
        let page = source(
            &f.kernel,
            ENTRIES,
            &[("concept", RELEASE), ("limit", "2"), ("as", face)],
            &reader("bobby"),
        );
        assert!(
            page.contains(&first) && page.contains(&second),
            "{face}: {page}"
        );
        if face == "application/atom+xml" {
            assert_eq!(page.matches("<entry>").count(), 2, "{page}");
        }
    }
}

#[test]
fn concept_and_author_filters_apply_before_the_page_limit() {
    let f = feed_fixture_with(
        "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\npublishers = [\"brian\", \"chris\"]\nscheme = \"releases.scheme.ttl\"\n",
        "[[mount]]\nlayer = \"releases\"\nbrian = [\"read\", \"say\"]\nbobby = [\"read\"]\nchris = [\"read\", \"say\"]\n",
    ).unwrap();
    publish(&f, "Brian first", &[RELEASE]);
    let content = format!("@prefix ik: <{IK}> . @prefix dcterms: <http://purl.org/dc/terms/> . <urn:layer:new> a ik:Message ; dcterms:title \"Chris second\" ; dcterms:subject <{RELEASE}> .");
    let chris = cap(&participant("chris"), &["urn:cap:layer:say:releases"]);
    let entry = text(
        &issue(
            &f.kernel,
            Verb::Sink,
            "urn:layer:releases:say",
            &[("content", &content)],
            &chris,
        )
        .unwrap(),
    );
    let page = source(
        &f.kernel,
        ENTRIES,
        &[
            ("concept", RELEASE),
            ("from", &participant("chris")),
            ("limit", "1"),
        ],
        &reader("bobby"),
    );
    assert!(page.contains(&entry), "{page}");
}
