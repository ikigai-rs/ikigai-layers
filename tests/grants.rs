//! The enrollment generator, and the manifold a generated grant projects.

mod common;

use std::collections::BTreeSet;
use std::process::Command;

use common::*;
use ikigai_core::{Capability, Kernel, Verb};
use ikigai_layers::grants::{generate, load_fingerprints, Enrollment};
use ikigai_layers::Config;

fn enrollment(config: &Scratch) -> Enrollment {
    let loaded = Config::load(config.path()).unwrap();
    let fingerprints = load_fingerprints(config.path(), &loaded).unwrap();
    generate(&loaded, &fingerprints).unwrap()
}

fn scopes(e: &Enrollment, grant: &str) -> Vec<String> {
    e.grants.get(grant).cloned().unwrap_or_default()
}

#[test]
fn each_participant_gets_the_union_of_its_manifests() {
    let config = Scratch::new("cfg");
    write_config(&config);
    let e = enrollment(&config);
    assert_eq!(
        scopes(&e, "brian"),
        vec![
            "urn:cap:collab:read:bobby",
            "urn:cap:collab:read:chris",
            "urn:cap:layer:feed:talk",
            "urn:cap:layer:feed:talk-bobby",
            "urn:cap:layer:read:notes",
            "urn:cap:layer:read:talk",
            "urn:cap:layer:read:talk-bobby",
            "urn:cap:layer:say:talk",
            "urn:cap:layer:say:talk-bobby",
        ]
    );
    assert_eq!(
        scopes(&e, "bobby-claude"),
        vec![
            "urn:cap:collab:read:bobby",
            "urn:cap:layer:feed:talk",
            "urn:cap:layer:read:talk",
            "urn:cap:layer:say:talk",
        ]
    );
}

/// Brian, 2026-10-10: the generator never emits a grant to another collaboration's layer.
#[test]
fn no_grant_reaches_another_collaborations_layer() {
    let config = Scratch::new("cfg");
    write_config(&config);
    let e = enrollment(&config);
    let chris = scopes(&e, "chris");
    assert!(
        chris
            .iter()
            .all(|s| !s.ends_with(":talk-bobby") && !s.ends_with(":bobby")),
        "{chris:?}"
    );
    for bobby_side in ["bobby", "bobby-claude", "hub"] {
        let held = scopes(&e, bobby_side);
        assert!(
            held.iter()
                .all(|s| !s.ends_with(":notes") && !s.ends_with(":chris")),
            "{bobby_side}: {held:?}"
        );
    }
    // And a manifest that tries is refused, so there is nothing to generate from.
    config.write(
        "collabs/chris.toml",
        "[[mount]]\nlayer = \"talk-bobby\"\nchris = [\"read\", \"say\"]\n",
    );
    assert!(Config::load(config.path()).is_err());
}

#[test]
fn clients_name_their_principal_and_nothing_defaults() {
    let config = Scratch::new("cfg");
    write_config(&config);
    let e = enrollment(&config);
    let json: serde_json::Value = serde_json::from_str(&e.clients_json()).unwrap();
    assert!(json.get("default").is_none(), "fail closed: {json}");
    let bobby =
        &json["clients"]["aa00000000000000000000000000000000000000000000000000000000000001"];
    assert_eq!(bobby["grant"], "bobby");
    assert_eq!(bobby["principal"], participant("bobby"));
    assert_eq!(bobby["label"], "Bobby");
    // The machine's certificate is read from its own name.
    let gonk_entry = &json["clients"][GONK_FP];
    assert_eq!(gonk_entry["principal"], gonk());
    assert_eq!(gonk_entry["grant"], format!("client.layers.{GONK_FP}"));
    // The hub holds a grant but has no certificate yet.
    assert_eq!(e.unenrolled, vec![participant("hub")]);
    let grants: serde_json::Value = serde_json::from_str(&e.grants_json()).unwrap();
    assert!(grants["bobby"].is_array());
}

#[test]
fn one_certificate_names_one_actor() {
    let config = Scratch::new("cfg");
    write_config(&config);
    config.write(
        "enrollment.toml",
        "bobby = \"aa00000000000000000000000000000000000000000000000000000000000001\"\nbrian = \"AA00000000000000000000000000000000000000000000000000000000000001\"\n",
    );
    let loaded = Config::load(config.path()).unwrap();
    let fingerprints = load_fingerprints(config.path(), &loaded).unwrap();
    let refused = generate(&loaded, &fingerprints).unwrap_err().to_string();
    assert!(
        refused.contains("one certificate names one actor"),
        "{refused}"
    );
    config.write(
        "enrollment.toml",
        "mallory = \"aa00000000000000000000000000000000000000000000000000000000000009\"\n",
    );
    assert!(load_fingerprints(config.path(), &loaded).is_err());
    config.write("enrollment.toml", "bobby = \"not-hex\"\n");
    assert!(load_fingerprints(config.path(), &loaded).is_err());
}

#[test]
fn the_binary_writes_both_files_or_nothing() {
    let config = Scratch::new("cfg");
    write_config(&config);
    let out = Scratch::new("out");
    let bin = env!("CARGO_BIN_EXE_ikigai-layers");
    let run = |args: &[&str]| {
        Command::new(bin)
            .args(args)
            .output()
            .expect("the binary runs")
    };

    let checked = run(&["grants", config.path().to_str().unwrap(), "--check"]);
    assert!(checked.status.success(), "{checked:?}");
    assert!(
        !config.path().join("grants.json").exists(),
        "--check writes nothing"
    );

    let written = run(&[
        "grants",
        config.path().to_str().unwrap(),
        "--out",
        out.path().to_str().unwrap(),
    ]);
    assert!(written.status.success(), "{written:?}");
    let grants: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.path().join("grants.json")).unwrap())
            .unwrap();
    assert!(grants["chris"].is_array());
    assert!(out.path().join("clients.json").exists());

    // A configuration that breaks isolation writes nothing, and says why.
    let broken = Scratch::new("cfg");
    write_config(&broken);
    broken.write(
        "collabs/chris.toml",
        "[[mount]]\nlayer = \"talk-bobby\"\nchris = [\"read\"]\n",
    );
    let out2 = Scratch::new("out");
    let refused = run(&[
        "grants",
        broken.path().to_str().unwrap(),
        "--out",
        out2.path().to_str().unwrap(),
    ]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("talk-bobby"));
    assert!(!out2.path().join("grants.json").exists());

    assert_eq!(run(&["frobnicate"]).status.code(), Some(2));
}

/// The actions a capability may invoke, by the same predicate the kernel's floor and
/// `urn:kernel:actions` use: every endpoint the kernel lists, every action it declares.
fn manifold(kernel: &Kernel, capability: &Capability) -> BTreeSet<String> {
    let mut offered = BTreeSet::new();
    for entry in kernel.entries().expect("an enumerable kernel") {
        let Some(description) = kernel.describe_pattern(&entry.pattern) else {
            continue;
        };
        for action in description.action_specs() {
            if description
                .unsatisfied_scopes(action.verb, capability)
                .is_empty()
                && !description.requires.is_empty()
            {
                offered.insert(format!("{}__{:?}", description.id, action.verb).to_lowercase());
            }
        }
    }
    offered
}

/// A participant's manifold under its GENERATED grant is exactly the layers it holds: the
/// MCP tool list is that manifold.
#[test]
fn a_generated_grant_projects_exactly_its_layers() {
    let f = fixture();
    let e = enrollment(&f.config);
    let grant = |name: &str| Capability::scoped(scopes(&e, name));

    let claude = manifold(&f.kernel, &grant("bobby-claude"));
    let expected: BTreeSet<String> = [
        "layer-talk-definition__source",
        "layer-talk-shapes__source",
        "layer-talk-context__source",
        "layer-talk-feed__source",
        "layer-talk-say__sink",
        "layer-talk-entries__source",
        "layer-talk-entry__source",
        "layer-talk-graph__source",
        "layer-talk-latest__source",
        "layers-feed__source",
        "collab__source",
        "collab-participants__source",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    assert_eq!(claude, expected);

    // The machine reads and follows talk-bobby and can say nothing anywhere; on the task
    // layer it holds every verb, each its own action of the one tuples door.
    let gonk_grant = grant(&format!("client.layers.{GONK_FP}"));
    let machine = manifold(&f.kernel, &gonk_grant);
    assert!(machine.contains("layer-talk-bobby-feed__source"));
    assert!(
        machine.iter().all(|a| !a.ends_with("-say__sink")),
        "{machine:?}"
    );
    for tool in [
        "layer-tasks-bobby-tuples__sink",
        "layer-tasks-bobby-tuples__source",
        "layer-tasks-bobby-tuples__delete",
        "layer-tasks-bobby-receipt__source",
        "layer-tasks-bobby-feed__source",
    ] {
        assert!(machine.contains(tool), "{tool}: {machine:?}");
    }
    // Bobby reads and follows the task layer, and neither drops nor takes.
    let bobby = manifold(&f.kernel, &grant("bobby"));
    assert!(
        bobby.contains("layer-tasks-bobby-tuples__source"),
        "{bobby:?}"
    );
    assert!(
        !bobby.contains("layer-tasks-bobby-tuples__sink")
            && !bobby.contains("layer-tasks-bobby-tuples__delete"),
        "{bobby:?}"
    );
    assert!(machine
        .iter()
        .all(|a| !a.starts_with("layer-talk-") || a.starts_with("layer-talk-bobby-")));

    // Nobody's generated grant reads the host-wide participants graph: that is the
    // operator's. Each reads its own collaborations' slice instead (`collab-participants`,
    // tests/door.rs).
    for name in e.grants.keys() {
        assert!(!manifold(&f.kernel, &grant(name)).contains("layers-participants__source"));
    }
    // Every tool name fits MCP's 64 characters.
    let all = manifold(&f.kernel, &Capability::root());
    assert!(all.iter().all(|a| a.len() <= 64), "{all:?}");

    // And the collaboration read checks the manifest's own token.
    let other = ikigai_core::Capability::scoped(scopes(&e, "chris"))
        .with_principal(&participant("chris"))
        .unwrap();
    let refused = issue(&f.kernel, Verb::Source, "urn:collab:bobby", &[], &other);
    assert!(
        matches!(refused, Err(ikigai_core::Error::Denied(_))),
        "{refused:?}"
    );
    let own = source(&f.kernel, "urn:collab:chris", &[], &other);
    assert!(
        own.contains("urn:layer:notes") && !own.contains("talk-bobby"),
        "{own}"
    );
}
