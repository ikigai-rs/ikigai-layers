//! The configuration resources, read-only (ledger #4307; `docs/design/runtime-layers.md`
//! section 2): every file the host reads served under `urn:layers:`, the derived grants and
//! clients, the revision, all under `urn:cap:layers:admin:read`, which only the `[admin]` table
//! of `layers.toml` grants.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::collections::BTreeSet;

use common::*;
use ikigai_core::{Capability, Error, Verb};
use ikigai_layers::config::{revision, CAP_ADMIN_READ};
use ikigai_layers::grants::{derive, generate, load_fingerprints};
use ikigai_layers::Config;

fn admin() -> Capability {
    cap(&participant("brian"), &[CAP_ADMIN_READ])
}

fn json(kernel: &ikigai_core::Kernel, iri: &str) -> serde_json::Value {
    serde_json::from_str(&source(
        kernel,
        iri,
        &[("as", "application/json")],
        &admin(),
    ))
    .unwrap_or_else(|e| panic!("{iri}: {e}"))
}

fn names(value: &serde_json::Value, set: &str) -> BTreeSet<String> {
    value[set]
        .as_array()
        .unwrap_or_else(|| panic!("{value}"))
        .iter()
        .map(|row| row["name"].as_str().unwrap().to_string())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

/// Every resource the item names, unkeyed and keyed, as the fixture fills them.
fn every_resource() -> Vec<&'static str> {
    vec![
        "urn:layers:layer",
        "urn:layers:layer:name:talk",
        "urn:layers:shape",
        "urn:layers:shape:name:msg",
        "urn:layers:context",
        "urn:layers:context:name:msg",
        "urn:layers:scheme",
        "urn:layers:collab",
        "urn:layers:collab:name:bobby",
        "urn:layers:collab:name:bobby:mount:layer:talk",
        "urn:layers:participant",
        "urn:layers:participant:name:bobby",
        "urn:layers:enrollment",
        "urn:layers:enrollment:fingerprint:aa00000000000000000000000000000000000000000000000000000000000003",
        "urn:layers:grant",
        "urn:layers:client",
        "urn:layers:revision",
    ]
}

#[test]
fn each_whole_file_atom_answers_its_file_byte_for_byte() {
    let f = fixture();
    let dir = f.config.path();
    // A comment and odd spacing survive: the answer is the file, not a rewriting of it.
    f.config.write(
        "layers/talk.toml",
        "# the shared room\nmodel   = \"channel\"\nlabel = \"Talk\"\nkind = \"talk\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\nshared = [\"bobby\", \"chris\"]\n",
    );
    for (iri, file) in [
        ("urn:layers:layer:name:talk", "layers/talk.toml"),
        (
            "urn:layers:layer:name:tasks-bobby",
            "layers/tasks-bobby.toml",
        ),
        ("urn:layers:shape:name:msg", "layers/msg.shapes.ttl"),
        ("urn:layers:shape:name:tasks", "layers/tasks.shapes.ttl"),
        ("urn:layers:context:name:msg", "layers/msg.context.jsonld"),
        (
            "urn:layers:context:name:tasks",
            "layers/tasks.context.jsonld",
        ),
        ("urn:layers:collab:name:bobby", "collabs/bobby.toml"),
        ("urn:layers:collab:name:chris", "collabs/chris.toml"),
    ] {
        let answer = issue(&f.kernel, Verb::Source, iri, &[], &admin())
            .unwrap_or_else(|e| panic!("{iri}: {e}"));
        assert_eq!(
            answer.bytes,
            std::fs::read(dir.join(file)).unwrap(),
            "{iri}"
        );
    }
    // A definition and a manifest have a JSON face with the file's keys.
    let talk = json(&f.kernel, "urn:layers:layer:name:talk");
    assert_eq!(talk["model"], "channel", "{talk}");
    assert_eq!(
        talk["shared"],
        serde_json::json!(["bobby", "chris"]),
        "{talk}"
    );
    let bobby = json(&f.kernel, "urn:layers:collab:name:bobby");
    assert_eq!(bobby["label"], "Bobby", "{bobby}");
    assert_eq!(bobby["mount"].as_array().unwrap().len(), 3, "{bobby}");

    // An atom nobody wrote is NotFound, naming it; a malformed key is the caller's error.
    for missing in [
        "urn:layers:layer:name:nope",
        "urn:layers:shape:name:nope",
        "urn:layers:collab:name:nope",
        "urn:layers:collab:name:chris:mount:layer:talk-bobby",
        "urn:layers:participant:name:nope",
        "urn:layers:enrollment:fingerprint:aa000000000000000000000000000000000000000000000000000000000000ff",
    ] {
        let refused = issue(&f.kernel, Verb::Source, missing, &[], &admin()).unwrap_err();
        assert!(matches!(refused, Error::NotFound(_)), "{missing}: {refused}");
    }
    for bad in [
        "urn:layers:layer:name:Talk",
        "urn:layers:collab:name:..",
        "urn:layers:enrollment:fingerprint:abc",
    ] {
        let refused = issue(&f.kernel, Verb::Source, bad, &[], &admin()).unwrap_err();
        assert!(
            matches!(refused, Error::InvalidArgument { .. }),
            "{bad}: {refused}"
        );
    }
}

#[test]
fn each_part_of_a_file_answers_that_part() {
    let f = fixture();
    // One mount: the verbs each participant holds on one layer.
    let mount = json(
        &f.kernel,
        "urn:layers:collab:name:bobby:mount:layer:talk-bobby",
    );
    assert_eq!(
        mount,
        serde_json::json!({
            "layer": "talk-bobby",
            "bobby": ["read", "feed", "say"],
            "brian": ["read", "feed", "say"],
            gonk(): ["read", "feed"],
        })
    );
    let toml_face = source(
        &f.kernel,
        "urn:layers:collab:name:bobby:mount:layer:talk-bobby",
        &[],
        &admin(),
    );
    let reparsed: toml::Table = toml_face.parse().unwrap();
    assert_eq!(
        serde_json::to_value(&reparsed).unwrap(),
        mount,
        "{toml_face}"
    );

    // One participant: its own statements in participants.ttl, and nobody else's.
    let claude = source(
        &f.kernel,
        "urn:layers:participant:name:bobby-claude",
        &[],
        &admin(),
    );
    let subjects: BTreeSet<String> = triples(&claude)
        .iter()
        .map(|t| t.subject.to_string())
        .collect();
    assert_eq!(
        subjects,
        set(&[&format!("<{}>", participant("bobby-claude"))])
    );
    let claude = json(&f.kernel, "urn:layers:participant:name:bobby-claude");
    assert_eq!(
        claude["onBehalfOf"],
        serde_json::json!([participant("bobby")]),
        "{claude}"
    );

    // One enrolled certificate, by its fingerprint in either spelling enrollment.toml takes.
    let bobby_fp = "aa00000000000000000000000000000000000000000000000000000000000001";
    let colons = "AA:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:01";
    for asked in [bobby_fp, colons] {
        let line = source(
            &f.kernel,
            &format!("urn:layers:enrollment:fingerprint:{asked}"),
            &[],
            &admin(),
        );
        assert_eq!(
            line,
            format!(
                "enrollment {bobby_fp} participant={}\n",
                participant("bobby")
            )
        );
    }
}

#[test]
fn each_set_lists_its_members() {
    let f = fixture();
    let layers = json(&f.kernel, "urn:layers:layer");
    assert_eq!(
        names(&layers, "layers"),
        set(&["notes", "talk", "talk-bobby", "tasks-bobby"])
    );
    let talk = layers["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["name"] == "talk")
        .unwrap();
    assert_eq!(talk["model"], "channel");
    assert_eq!(talk["kind"], "talk");
    assert_eq!(talk["mountedBy"], serde_json::json!(["bobby", "chris"]));
    assert_eq!(talk["resource"], "urn:layers:layer:name:talk");
    let plain = source(&f.kernel, "urn:layers:layer", &[], &admin());
    assert!(
        plain.contains(
            "layer urn:layers:layer:name:talk model=channel kind=talk mounted-by=bobby,chris\n"
        ),
        "{plain}"
    );
    assert!(
        plain.contains(
            "layer urn:layers:layer:name:notes model=channel kind=none mounted-by=chris\n"
        ),
        "{plain}"
    );

    let shapes = json(&f.kernel, "urn:layers:shape");
    assert_eq!(names(&shapes, "shapes"), set(&["msg", "tasks"]));
    let msg = &shapes["shapes"][0];
    assert_eq!(msg["file"], "layers/msg.shapes.ttl");
    assert_eq!(
        msg["layers"],
        serde_json::json!(["notes", "talk", "talk-bobby"])
    );
    assert_eq!(
        names(&json(&f.kernel, "urn:layers:context"), "contexts"),
        set(&["msg", "tasks"])
    );
    // No feed here, so no scheme: an empty set, not a refusal.
    assert_eq!(
        json(&f.kernel, "urn:layers:scheme"),
        serde_json::json!({"schemes": []})
    );

    let collabs = json(&f.kernel, "urn:layers:collab");
    assert_eq!(names(&collabs, "collabs"), set(&["bobby", "chris"]));
    let plain = source(&f.kernel, "urn:layers:collab", &[], &admin());
    assert!(
        plain.contains("collab urn:layers:collab:name:chris mounts=talk,notes\n"),
        "{plain}"
    );
    assert!(
        plain.contains("mount urn:layers:collab:name:chris:mount:layer:notes\n"),
        "{plain}"
    );

    // The participant set is the participants graph, as urn:layers:participants answers it.
    let all = source(&f.kernel, "urn:layers:participant", &[], &admin());
    let roster = source(
        &f.kernel,
        "urn:layers:participants",
        &[],
        &Capability::root(),
    );
    assert_eq!(all, roster);
    let listed = json(&f.kernel, "urn:layers:participant");
    let members: BTreeSet<String> = listed["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["participant"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(members.len(), 7, "{listed}");
    assert!(members.contains(&gonk()), "{listed}");

    let enrolled = json(&f.kernel, "urn:layers:enrollment");
    let fps: BTreeSet<String> = enrolled["enrollments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["fingerprint"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(fps.len(), 5, "{enrolled}");
}

#[test]
fn the_grants_and_clients_are_what_the_generator_derives() {
    let f = fixture();
    let config = Config::load(f.config.path()).unwrap();
    let fingerprints = load_fingerprints(f.config.path(), &config).unwrap();
    let enrollment = generate(&config, &fingerprints).unwrap();
    assert_eq!(
        source(&f.kernel, "urn:layers:grant", &[], &admin()),
        enrollment.grants_json()
    );
    assert_eq!(
        source(&f.kernel, "urn:layers:client", &[], &admin()),
        enrollment.clients_json()
    );
    // Derived from the files now: a new grant in a manifest shows at once, nothing written.
    f.config.write(
        "collabs/chris.toml",
        &format!("{CHRIS_TOML}bobby = [\"read\"]\n"),
    );
    let grants: serde_json::Value =
        serde_json::from_str(&source(&f.kernel, "urn:layers:grant", &[], &admin())).unwrap();
    assert!(
        grants["bobby"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("urn:cap:layer:read:notes")),
        "{grants}"
    );
    assert!(!f.config.path().join("grants.json").exists());
}

#[test]
fn the_revision_changes_when_a_read_file_changes_and_not_otherwise() {
    let f = fixture();
    let read = |f: &Fixture| {
        let plain = source(&f.kernel, "urn:layers:revision", &[], &admin());
        let revision = plain
            .strip_prefix("revision ")
            .and_then(|r| r.strip_suffix('\n'))
            .unwrap_or_else(|| panic!("{plain:?}"))
            .to_string();
        assert_eq!(revision.len(), 64, "{plain}");
        let json = json(&f.kernel, "urn:layers:revision");
        assert_eq!(json, serde_json::json!({ "revision": revision }));
        revision
    };
    let first = read(&f);
    assert_eq!(read(&f), first, "reading twice changes nothing");
    assert_eq!(revision(f.config.path()).unwrap(), first);

    // Files the configuration does not read change nothing.
    f.config
        .write("layers/unnamed.shapes.ttl", "# no definition names this\n");
    f.config.write("notes.txt", "operator's notes\n");
    f.config.write("grants.json", "{}\n");
    f.config.write("collabs/README.md", "not a manifest\n");
    assert_eq!(read(&f), first, "a file the configuration does not read");

    // Every file it does read changes it, and putting the bytes back restores it.
    let originals: Vec<(&str, Option<Vec<u8>>)> = [
        "layers.toml",
        "layers/talk.toml",
        "layers/msg.shapes.ttl",
        "layers/tasks.context.jsonld",
        "participants.ttl",
        "collabs/bobby.toml",
        "enrollment.toml",
        "entitlements.toml",
    ]
    .into_iter()
    .map(|file| (file, std::fs::read(f.config.path().join(file)).ok()))
    .collect();
    let mut seen = BTreeSet::from([first.clone()]);
    for (file, original) in &originals {
        let path = f.config.path().join(file);
        let mut changed = original.clone().unwrap_or_default();
        changed.extend_from_slice(b"\n# changed\n");
        std::fs::write(&path, &changed).unwrap();
        let now = read(&f);
        assert!(seen.insert(now), "{file}: the revision did not change");
        match original {
            Some(bytes) => std::fs::write(&path, bytes).unwrap(),
            None => std::fs::remove_file(&path).unwrap(),
        }
        assert_eq!(read(&f), first, "{file}: restored");
    }
    // A new definition or manifest is a file read too, and so is a document it names.
    f.config.write(
        "layers/extra.toml",
        "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"extra.context.jsonld\"\n",
    );
    let with_extra = read(&f);
    assert_ne!(with_extra, first);
    f.config
        .write("layers/extra.context.jsonld", "{ \"@context\": {} }\n");
    assert_ne!(read(&f), with_extra, "a document a new definition names");
    f.config.remove("layers/extra.toml");
    f.config.remove("layers/extra.context.jsonld");
    assert_eq!(read(&f), first);
}

#[test]
fn layer_verbs_without_an_admin_grant_read_nothing() {
    let f = fixture();
    let e = derive(f.config.path()).unwrap().1;
    // Every generated grant: the widest is brian's, read, feed and say across collaborations.
    for (slug, scopes) in &e.grants {
        let held = cap(
            &participant(slug),
            &scopes.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        for iri in every_resource() {
            let refused = issue(&f.kernel, Verb::Source, iri, &[], &held).unwrap_err();
            assert!(
                matches!(refused, Error::Denied(_)),
                "{slug} on {iri}: {refused}"
            );
        }
    }
    // A participant of every fixture layer is refused too, and an admin grant alone reads all.
    for iri in every_resource() {
        let refused = issue(
            &f.kernel,
            Verb::Source,
            iri,
            &[],
            &member(&participant("bobby")),
        )
        .unwrap_err();
        assert!(matches!(refused, Error::Denied(_)), "{iri}: {refused}");
        issue(&f.kernel, Verb::Source, iri, &[], &admin()).unwrap_or_else(|e| panic!("{iri}: {e}"));
    }
    // And the admin grant reads no layer: administering is not taking part.
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk:entries",
        &[],
        &admin(),
    )
    .unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused}");
}

#[test]
fn every_admin_read_declares_the_admin_capability() {
    let f = fixture();
    for iri in every_resource() {
        let meta = issue(
            &f.kernel,
            Verb::Meta,
            iri,
            &[("as", "application/json")],
            &Capability::root(),
        )
        .unwrap_or_else(|e| panic!("Meta {iri}: {e}"));
        let meta = String::from_utf8(meta.bytes).unwrap();
        assert!(meta.contains(CAP_ADMIN_READ), "{iri}: {meta}");
    }
    // Read-only: a write to any of them is refused, and nothing is written.
    for iri in [
        "urn:layers:layer",
        "urn:layers:layer:name:talk",
        "urn:layers:revision",
    ] {
        let refused = issue(
            &f.kernel,
            Verb::Sink,
            iri,
            &[("content", "x")],
            &Capability::root(),
        )
        .unwrap_err();
        assert!(
            refused.to_string().contains("read-only"),
            "{iri}: {refused}"
        );
    }
}

#[test]
fn the_admin_table_gives_admin_read_to_the_participants_it_names() {
    let f = fixture();
    f.config
        .write("layers.toml", "[admin]\nread = [\"brian\", \"hub\"]\n");
    let (_, e) = derive(f.config.path()).unwrap();
    for slug in ["brian", "hub"] {
        assert!(
            e.grants[slug].contains(&CAP_ADMIN_READ.to_string()),
            "{slug}: {:?}",
            e.grants[slug]
        );
    }
    for (slug, scopes) in &e.grants {
        if !["brian", "hub"].contains(&slug.as_str()) {
            assert!(
                !scopes.iter().any(|s| s.contains(":admin:")),
                "{slug}: {scopes:?}"
            );
        }
    }
    assert_eq!(
        e.admins.keys().cloned().collect::<Vec<_>>(),
        ["brian", "hub"]
    );
    // The host still opens over the table, and brian's generated grant reads the resources.
    let mut f = f;
    f.reopen();
    let brian = cap(
        &participant("brian"),
        &e.grants["brian"]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    );
    for iri in every_resource() {
        issue(&f.kernel, Verb::Source, iri, &[], &brian).unwrap_or_else(|e| panic!("{iri}: {e}"));
    }

    // Refused, naming what to do: a key that is no capability, an unknown participant.
    for (table, why) in [
        ("[admin]\nwrite = [\"brian\"]\n", "has no key `write`"),
        (
            "[admin]\nread = \"brian\"\n",
            "is a list of participant names",
        ),
        ("[admin]\nread = [\"nobody\"]\n", "which is no participant"),
        ("admin = 1\n", "is a table"),
    ] {
        f.config.write("layers.toml", table);
        let refused = derive(f.config.path()).unwrap_err().to_string();
        assert!(refused.contains(why), "{table}: {refused}");
        assert!(refused.contains("nothing written"), "{table}: {refused}");
    }
}

#[test]
fn a_manifest_naming_an_admin_capability_is_refused_by_the_generator() {
    for (manifest, what) in [
        (CHRIS_TOML.to_string(), None),
        (
            CHRIS_TOML.replace(
                "brian = [\"read\"]",
                "brian = [\"read\", \"urn:cap:layers:admin:read\"]",
            ),
            Some("`brian` holding `urn:cap:layers:admin:read` on `notes`"),
        ),
        (
            CHRIS_TOML.replace("brian = [\"read\"]", "brian = [\"admin:read\"]"),
            Some("`brian` holding `admin:read` on `notes`"),
        ),
        (
            format!("admin = [\"chris\"]\n{CHRIS_TOML}"),
            Some("`admin`"),
        ),
    ] {
        let config = Scratch::new("admin-manifest");
        write_config(&config);
        config.write("collabs/chris.toml", &manifest);
        let derived = derive(config.path());
        match what {
            None => {
                derived.unwrap();
            }
            Some(what) => {
                let refused = derived.unwrap_err().to_string();
                assert!(refused.contains(what), "{refused}");
                assert!(
                    refused.contains("no manifest grants one")
                        && refused.contains("[admin] table of layers.toml"),
                    "{refused}"
                );
                assert!(!config.path().join("grants.json").exists());
            }
        }
    }
}

#[test]
fn the_participants_roster_answers_as_before() {
    let f = fixture();
    let roster = source(
        &f.kernel,
        "urn:layers:participants",
        &[],
        &Capability::root(),
    );
    assert_eq!(
        roster,
        ikigai_layers::rdf::to_turtle(
            &ikigai_layers::config::load_participants(f.config.path())
                .unwrap()
                .triples
        )
    );
    // Under its own capability, unchanged: admin:read does not read it, and its holder does.
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:layers:participants",
        &[],
        &admin(),
    )
    .unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused}");
    let reader = cap(
        &participant("brian"),
        &[ikigai_layers::endpoints::CAP_PARTICIPANTS],
    );
    assert_eq!(
        source(&f.kernel, "urn:layers:participants", &[], &reader),
        roster
    );
}

#[test]
fn grants_prints_one_admin_line_per_administrator() {
    let config = Scratch::new("admin-binary");
    write_config(&config);
    config.write("layers.toml", "[admin]\nread = [\"brian\"]\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-layers"))
        .args(["grants", "--check"])
        .arg(config.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success(), "{stdout}");
    let lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with("admin ")).collect();
    assert_eq!(
        lines,
        ["admin brian: gains urn:cap:layers:admin:read, administering this host's configuration ([admin])"],
        "{stdout}"
    );
    assert!(stdout.contains("urn:cap:layers:admin:read"), "{stdout}");

    // A manifest naming one: refused on stderr, exit 1, nothing written.
    config.write(
        "collabs/chris.toml",
        &CHRIS_TOML.replace("brian = [\"read\"]", "brian = [\"admin:read\"]"),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_ikigai-layers"))
        .arg("grants")
        .arg(config.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("no manifest grants one"), "{stderr}");
    assert!(!config.path().join("grants.json").exists());
}

// A feed, for its scheme: the same definition `tests/feed_layers.rs` opens.
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

#[test]
fn a_feed_scheme_is_served_byte_for_byte() {
    let config = Scratch::new("admin-feed");
    write_config(&config);
    config.write("layers/feed.shapes.ttl", FEED_SHAPES);
    config.write("layers/feed.context.jsonld", FEED_CONTEXT);
    config.write("layers/releases.scheme.ttl", SCHEME_TTL);
    config.write(
        "layers/releases.toml",
        "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\npublishers = [\"brian\"]\nscheme = \"releases.scheme.ttl\"\n",
    );
    let state = Scratch::new("admin-feed-state");
    let host = ikigai_layers::Host::open(config.path(), state.path(), &key(config.path()))
        .expect("the feed configuration opens");
    let kernel = kernel_over(&host);
    let schemes = json(&kernel, "urn:layers:scheme");
    assert_eq!(
        schemes,
        serde_json::json!({"schemes": [{
            "name": "releases",
            "resource": "urn:layers:scheme:name:releases",
            "file": "layers/releases.scheme.ttl",
            "layers": ["releases"],
        }]})
    );
    let answer = issue(
        &kernel,
        Verb::Source,
        "urn:layers:scheme:name:releases",
        &[],
        &admin(),
    )
    .unwrap();
    assert_eq!(answer.bytes, SCHEME_TTL.as_bytes());
    assert_eq!(
        source(&kernel, "urn:layers:scheme", &[], &admin()),
        "scheme urn:layers:scheme:name:releases file=layers/releases.scheme.ttl layers=releases\n"
    );
}
