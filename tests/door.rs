//! What a door runs, under the capabilities a door mints: each participant's GENERATED grant,
//! with its principal. Three things a first install needs to hold (ledger #1282, #1267,
//! #1266): a participant can learn who its collaborators are, one state directory has one
//! host, and admission needs no validator in the door's kernel.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::*;
use ikigai_core::{Capability, Error, Fallback, Kernel, Space, Verb};
use ikigai_layers::grants::{generate, load_fingerprints, Enrollment};
use ikigai_layers::{Config, Host};

fn enrollment(config: &Scratch) -> Enrollment {
    let loaded = Config::load(config.path()).unwrap();
    let fingerprints = load_fingerprints(config.path(), &loaded).unwrap();
    generate(&loaded, &fingerprints).unwrap()
}

/// The capability a door mints for `participant` (a slug in this fixture), from its
/// generated grant.
fn minted(e: &Enrollment, slug: &str) -> Capability {
    cap(
        &participant(slug),
        &e.grants[slug]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
    )
}

/// The subjects of a Turtle document.
fn subjects(turtle: &str) -> BTreeSet<String> {
    triples(turtle)
        .into_iter()
        .map(|t| t.subject.to_string().trim_matches(['<', '>']).to_string())
        .collect()
}

/// The actions a capability may invoke, by the predicate `urn:kernel:actions` uses.
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
            {
                offered.insert(format!("{}__{:?}", description.id, action.verb).to_lowercase());
            }
        }
    }
    offered
}

// ---- ledger #1282: who reads the participants graph ------------------------------------

/// Every participant of a collaboration reads THAT collaboration's participants (each one's
/// kind, label and for whom it acts) under the grant it already holds, and nothing of
/// another collaboration's.
#[test]
fn a_participant_reads_its_collaborations_participants_through_a_door() {
    let f = fixture();
    let e = enrollment(&f.config);

    // The host-wide graph stays the operator's: no generated grant reads it.
    let claude = minted(&e, "bobby-claude");
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:layers:participants",
        &[],
        &claude,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");

    let bobby = source(&f.kernel, "urn:collab:bobby:participants", &[], &claude);
    let expected: BTreeSet<String> = [
        participant("bobby"),
        participant("bobby-claude"),
        participant("bobby-gonk"),
        participant("brian"),
        participant("hub"),
        gonk(),
    ]
    .into_iter()
    .collect();
    assert_eq!(subjects(&bobby), expected, "{bobby}");
    // What a mirror needs from it: each participant's kind, and for whom an agent acts.
    let facts: Vec<String> = triples(&bobby).iter().map(ToString::to_string).collect();
    assert!(
        facts.contains(&format!(
            "<{}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://xmlns.com/foaf/0.1/Person>",
            participant("bobby")
        )),
        "{facts:?}"
    );
    assert!(
        facts.contains(&format!(
            "<{}> <http://www.w3.org/ns/prov#actedOnBehalfOf> <{}>",
            participant("hub"),
            participant("brian")
        )),
        "{facts:?}"
    );
    assert!(!bobby.contains("Chris"), "{bobby}");

    // Chris takes part in `chris` only: he reads its participants, and bobby's are refused
    // exactly as bobby's manifest is.
    let chris = minted(&e, "chris");
    let own = source(&f.kernel, "urn:collab:chris:participants", &[], &chris);
    assert_eq!(
        subjects(&own),
        [participant("brian"), participant("chris")]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "{own}"
    );
    let other = issue(
        &f.kernel,
        Verb::Source,
        "urn:collab:bobby:participants",
        &[],
        &chris,
    );
    assert!(matches!(other, Err(Error::Denied(_))), "{other:?}");

    // Every generated grant is offered the action (the MCP tool list is this manifold).
    for slug in [
        "bobby",
        "bobby-claude",
        "bobby-gonk",
        "brian",
        "hub",
        "chris",
    ] {
        assert!(
            manifold(&f.kernel, &minted(&e, slug)).contains("collab-participants__source"),
            "{slug}"
        );
    }

    // And the manifest itself still answers at its own name.
    let manifest = source(&f.kernel, "urn:collab:bobby", &[], &claude);
    assert!(manifest.contains("urn:layer:talk-bobby"), "{manifest}");
}

/// A collaboration that does not exist is refused like one the caller does not hold, so the
/// refusal names nothing.
#[test]
fn an_unknown_collaborations_participants_are_refused_without_naming_it() {
    let f = fixture();
    let e = enrollment(&f.config);
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:collab:nobody:participants",
        &[],
        &minted(&e, "bobby"),
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    // Root holds every collaboration's read: an absent one is then NotFound.
    let absent = issue(
        &f.kernel,
        Verb::Source,
        "urn:collab:nobody:participants",
        &[],
        &Capability::root(),
    );
    assert!(matches!(absent, Err(Error::NotFound(_))), "{absent:?}");
}

// ---- ledger #1267: one host per state directory ----------------------------------------

#[test]
fn a_second_host_over_one_state_directory_is_refused_naming_the_first() {
    let f = fixture();
    let second = Host::open(
        f.config.path(),
        f.state.path(),
        &common::key(f.config.path()),
    );
    let Err(e) = second else {
        panic!("a second host opened the state directory the first holds");
    };
    let message = e.to_string();
    assert!(
        message.contains(&format!("pid {}", std::process::id())),
        "{message}"
    );
    assert!(
        message.contains(&f.state.path().display().to_string()),
        "{message}"
    );

    // The first host is untouched and still writes.
    say(&f.kernel, "talk", &participant("bobby"), &message_ttl("x")).expect("the first host");

    // Once the first host is gone, the directory opens again, and what it wrote is there.
    let common::Fixture {
        config,
        state,
        host,
        kernel,
    } = f;
    drop(kernel);
    drop(host);
    let again = Host::open(config.path(), state.path(), &common::key(config.path()))
        .expect("the lock went with the host");
    assert_eq!(again.layer("talk").unwrap().last_seq(), 1);
}

/// Another state directory is another host: the lock is per directory, not per process.
#[test]
fn two_hosts_over_two_state_directories_both_open() {
    let f = fixture();
    let other = Scratch::new("state-b");
    let second = Host::open(f.config.path(), other.path(), &common::key(f.config.path()))
        .expect("a second directory");
    assert!(second.layer("talk").is_some());
}

// ---- ledger #1266: the door's kernel binds no validator --------------------------------

/// The door binds the layer space ALONE: admission still admits what conforms and refuses
/// what does not, and no participant's manifold carries `urn:shacl:validate`. Bound beside,
/// as a door built for 0.2.0 binds it, the validator is offered to every participant,
/// because it declares no capability: that is the exposure this removes the need for.
#[test]
fn admission_needs_no_validator_in_the_doors_kernel() {
    let f = fixture();
    let e = enrollment(&f.config);
    let door = kernel_over(&f.host);

    let said = say(&door, "talk", &participant("bobby"), &message_ttl("hello"))
        .expect("a conforming entry is admitted without a validator bound");
    assert!(said.starts_with("urn:layer:talk:entry:"), "{said}");

    let bodiless = format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message .\n");
    let refused = say(&door, "talk", &participant("bobby"), &bodiless);
    assert!(
        matches!(refused, Err(Error::InvalidArgument { ref detail, .. }) if detail.contains("does not conform")),
        "{refused:?}"
    );
    assert_eq!(f.host.layer("talk").unwrap().last_seq(), 1);

    let beside = Kernel::with_meta_renderer(
        Arc::new(Fallback::new(vec![
            Arc::new(ikigai_layers::space(&f.host)) as Arc<dyn Space>,
            Arc::new(ikigai_shacl::space()) as Arc<dyn Space>,
        ])),
        Arc::new(ikigai_vocab::TurtleRenderer),
    );
    for slug in e.grants.keys() {
        let grant = Capability::scoped(e.grants[slug].clone());
        let tools = manifold(&door, &grant);
        assert!(
            tools.iter().all(|t| !t.starts_with("shacl-")),
            "{slug}: {tools:?}"
        );
        assert!(
            manifold(&beside, &grant).contains("shacl-validate__source"),
            "{slug}"
        );
    }
}

fn message_ttl(body: &str) -> String {
    message(body)
}
