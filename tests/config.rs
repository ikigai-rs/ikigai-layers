//! Configuration: what an operator may write, and the isolation rule.

mod common;

use common::*;
use ikigai_layers::{Config, Host};

fn refused(config: &Scratch) -> String {
    match Config::load(config.path()) {
        Ok(_) => panic!("the configuration should be refused"),
        Err(e) => e.to_string(),
    }
}

fn layer(model: &str, extra: &str) -> String {
    format!(
        "model = \"{model}\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\n{extra}"
    )
}

#[test]
fn the_fixture_loads() {
    let config = Scratch::new("cfg");
    write_config(&config);
    let loaded = Config::load(config.path()).unwrap();
    assert_eq!(
        loaded.layers.keys().collect::<Vec<_>>(),
        ["notes", "talk", "talk-bobby", "tasks-bobby"]
    );
    assert_eq!(
        loaded.collabs.keys().collect::<Vec<_>>(),
        ["bobby", "chris"]
    );
    assert_eq!(loaded.participants.members.len(), 7);
    // `match` is off unless a definition turns it on (ledger #1327).
    assert!(loaded.layers.values().all(|def| !def.matching));
}

/// `match = true` turns a tuplespace's templates on, and is read as written.
#[test]
fn match_is_a_tuplespace_option_off_by_default() {
    let config = Scratch::new("cfg");
    write_config(&config);
    config.write("layers/extra.toml", &layer("tuplespace", "match = true\n"));
    config.write("layers/quiet.toml", &layer("tuplespace", "match = false\n"));
    let loaded = Config::load(config.path()).unwrap();
    assert!(loaded.layers["extra"].matching);
    assert!(!loaded.layers["quiet"].matching);
    assert!(!loaded.layers["tasks-bobby"].matching);
}

/// Brian, 2026-10-10: a manifest naming a layer another manifest names is REFUSED unless
/// the layer declares itself shared with exactly those collaborations.
#[test]
fn a_private_layer_mounted_by_two_collaborations_is_refused() {
    let config = Scratch::new("cfg");
    write_config(&config);
    config.write(
        "collabs/chris.toml",
        "[[mount]]\nlayer = \"talk-bobby\"\nchris = [\"read\"]\n",
    );
    let message = refused(&config);
    assert!(
        message.contains("`talk-bobby`") && message.contains("declares no `shared` list"),
        "{message}"
    );
    // The host will not open over it either.
    let state = Scratch::new("state");
    assert!(Host::open(config.path(), state.path()).is_err());
}

#[test]
fn a_shared_layer_admits_only_the_collaborations_it_lists() {
    let config = Scratch::new("cfg");
    write_config(&config);
    // talk is shared with bobby and chris; a third collaboration may not mount it.
    config.write(
        "collabs/david.toml",
        "[[mount]]\nlayer = \"talk\"\nbrian = [\"read\"]\n",
    );
    let message = refused(&config);
    assert!(
        message.contains("shared with `bobby`, `chris` only") && message.contains("`david`"),
        "{message}"
    );
    // Listing it is the recorded decision that makes it legal.
    config.write(
        "layers/talk.toml",
        &layer("channel", "shared = [\"bobby\", \"chris\", \"david\"]\n"),
    );
    Config::load(config.path()).unwrap();
}

#[test]
fn a_shared_list_binds_even_a_single_mount() {
    let config = Scratch::new("cfg");
    write_config(&config);
    config.write(
        "layers/talk.toml",
        &layer("channel", "shared = [\"bobby\"]\n"),
    );
    let message = refused(&config);
    assert!(message.contains("`chris`"), "{message}");
}

#[test]
fn layer_names_fit_a_tool_name() {
    let config = Scratch::new("cfg");
    write_config(&config);
    config.write(
        "layers/abcdefghijklmnopqrstuvwxy.toml",
        &layer("channel", ""),
    );
    assert!(refused(&config).contains("is not a layer name"));
    config.remove("layers/abcdefghijklmnopqrstuvwxy.toml");
    config.write(
        "layers/abcdefghijklmnopqrstuvwx.toml",
        &layer("channel", ""),
    );
    Config::load(config.path()).unwrap();
}

#[test]
fn only_built_models_load() {
    for (model, says) in [("graph", "not built"), ("crdt", "unknown model")] {
        let config = Scratch::new("cfg");
        write_config(&config);
        config.write("layers/extra.toml", &layer(model, ""));
        let message = refused(&config);
        assert!(message.contains(says), "{model}: {message}");
    }
}

#[test]
fn a_definition_may_not_say_what_it_does_not_mean() {
    let cases = [
        // A typo'd key is refused, not ignored.
        (layer("channel", "adressed = true\n"), "unknown field"),
        // A file outside layers/.
        (
            "model = \"channel\"\nshapes = \"../participants.ttl\"\ncontext = \"msg.context.jsonld\"\n"
                .to_string(),
            "not a path",
        ),
        (layer("channel", "entry_max = \"2GiB\"\n"), "unit"),
        (layer("channel", "entry_max = \"4MiB\"\n"), "outside"),
        (layer("channel", "shared = [\"Bob\"]\n"), "no collaboration name"),
        // An option of another model says something the layer does not mean.
        (layer("channel", "addressed = true\n"), "a tuplespace option"),
        (layer("channel", "match = false\n"), "a tuplespace option"),
        (
            layer("tuplespace", "about = \"urn:example:p\"\n"),
            "a channel option",
        ),
        (layer("tuplespace", "addressed = \"yes\"\n"), "invalid type"),
        (layer("tuplespace", "match = \"yes\"\n"), "invalid type"),
    ];
    for (text, says) in cases {
        let config = Scratch::new("cfg");
        write_config(&config);
        config.write("layers/extra.toml", &text);
        let message = refused(&config);
        assert!(message.contains(says), "{text}: {message}");
    }
}

#[test]
fn shapes_must_be_closed_and_target_a_class() {
    let open = SHAPES.replacen("sh:closed true ;", "", 1);
    let untargeted = "@prefix sh: <http://www.w3.org/ns/shacl#> .\n<urn:x:s> a sh:NodeShape ; sh:closed true .\n";
    for (shapes, says) in [
        (open.as_str(), "not closed"),
        (untargeted, "targets a class"),
    ] {
        let config = Scratch::new("cfg");
        write_config(&config);
        config.write("layers/odd.shapes.ttl", shapes);
        config.write(
            "layers/extra.toml",
            "model = \"channel\"\nshapes = \"odd.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\n",
        );
        let message = refused(&config);
        assert!(message.contains(says), "{message}");
    }
}

#[test]
fn a_context_is_an_inline_allowlist() {
    for (context, says) in [
        (
            r#"{"@context": {"@vocab": "https://ikigai-rs.dev/ns#"}}"#,
            "@vocab",
        ),
        (r#"{"@context": {"@base": "urn:x:"}}"#, "@base"),
        (
            r#"{"@context": "https://example.org/context.jsonld"}"#,
            "remote context",
        ),
        (
            r#"{"@context": {"t": {"@id": "urn:x:t", "@context": {"@vocab": "urn:x:"}}}}"#,
            "@vocab",
        ),
        (
            r#"{"@context": [{"a": "urn:x:a"}, "https://example.org/c"]}"#,
            "remote context",
        ),
        (r#"{"nothing": 1}"#, "no `@context`"),
    ] {
        let config = Scratch::new("cfg");
        write_config(&config);
        config.write("layers/odd.context.jsonld", context);
        config.write(
            "layers/extra.toml",
            "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"odd.context.jsonld\"\n",
        );
        let message = refused(&config);
        assert!(message.contains(says), "{context}: {message}");
    }
}

#[test]
fn a_manifest_names_only_what_exists() {
    for (manifest, says) in [
        ("[[mount]]\nlayer = \"nowhere\"\nbobby = [\"read\"]\n", "no definition defines"),
        ("[[mount]]\nlayer = \"talk-bobby\"\nmallory = [\"read\"]\n", "does not name"),
        ("[[mount]]\nlayer = \"talk-bobby\"\nbobby = [\"take\"]\n", "whose verbs are"),
        (
            "[[mount]]\nlayer = \"talk-bobby\"\nbobby = [\"read\"]\n[[mount]]\nlayer = \"talk-bobby\"\nbrian = [\"read\"]\n",
            "mounted twice",
        ),
        ("owner = \"bobby\"\n", "unknown key"),
    ] {
        let config = Scratch::new("cfg");
        write_config(&config);
        config.write("collabs/bobby.toml", manifest);
        let message = refused(&config);
        assert!(message.contains(says), "{manifest}: {message}");
    }
}

#[test]
fn participants_are_named_and_typed() {
    for (graph, says) in [
        ("<urn:example:alice> a <http://xmlns.com/foaf/0.1/Person> .\n", "a participant is"),
        (
            "<urn:collab:participant:alice> <http://www.w3.org/2000/01/rdf-schema#label> \"A\" .\n",
            "no rdf:type",
        ),
        (
            "<urn:collab:participant:alice> a <http://xmlns.com/foaf/0.1/Person> ; <http://xmlns.com/foaf/0.1/knows> [] .\n",
            "blank node",
        ),
    ] {
        let config = Scratch::new("cfg");
        write_config(&config);
        config.write("collabs/bobby.toml", "");
        config.write("collabs/chris.toml", "");
        config.write("participants.ttl", graph);
        let message = refused(&config);
        assert!(message.contains(says), "{graph}: {message}");
    }
}
