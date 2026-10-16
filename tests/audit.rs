//! Reproductions from the unled audit of 2026-10-11 (ledger #1318 and the audit brief): each
//! test failed on the code it names before its fix, through the in-process door the other
//! suites use (a kernel binding the layer space alone, principals minted as a door mints
//! them).

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use common::*;
use ikigai_core::{Capability, Verb};

/// Every string in a JSON value, depth first.
fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

/// Ledger #1318: `collab-participants`'s summary lost its line continuations, so the
/// published summary carried runs of 17 spaces into the catalog and every MCP tool
/// description projected from it. Every door's self-description is checked, not just that
/// one: the same slip anywhere else reaches the same places.
#[test]
fn no_self_description_carries_a_run_of_spaces() {
    let f = fixture();
    let mut iris = vec![
        "urn:layers:feed".to_string(),
        "urn:layers:participants".to_string(),
        "urn:layers:me".to_string(),
        "urn:collab:bobby".to_string(),
        "urn:collab:bobby:participants".to_string(),
    ];
    for layer in ["talk", "talk-bobby", "notes"] {
        for part in [
            "",
            ":shapes",
            ":context",
            ":feed",
            ":say",
            ":entries",
            ":entry:x",
            ":graph:bobby",
            ":latest",
        ] {
            iris.push(format!("urn:layer:{layer}{part}"));
        }
    }
    for part in ["", ":shapes", ":context", ":feed", ":tuples", ":receipt:x"] {
        iris.push(format!("urn:layer:{TASKS}{part}"));
    }
    for iri in &iris {
        let meta = issue(
            &f.kernel,
            Verb::Meta,
            iri,
            &[("as", "application/json")],
            &Capability::root(),
        )
        .unwrap_or_else(|e| panic!("Meta {iri}: {e}"));
        let json: serde_json::Value =
            serde_json::from_slice(&meta.bytes).unwrap_or_else(|e| panic!("{iri}: {e}"));
        let mut found = Vec::new();
        strings(&json, &mut found);
        for s in found {
            assert!(
                !s.contains("  "),
                "{iri}'s self-description carries a run of spaces (a lost `\\` line \
                 continuation): {s:?}"
            );
        }
    }
}

/// The same slip in the refusal a second host over a held state directory reads (found
/// beside ledger #1318).
#[test]
fn the_held_state_directory_refusal_carries_no_run_of_spaces() {
    let f = fixture();
    let refusal = match ikigai_layers::Host::open(f.config.path(), f.state.path()) {
        Ok(_) => panic!("a second host over a held state directory opened"),
        Err(e) => e.to_string(),
    };
    assert!(
        refusal.contains("one host per state directory"),
        "the refusal reads: {refusal}"
    );
    assert!(!refusal.contains("  "), "the refusal reads: {refusal}");
}

/// A feed read that waits and times out on a quiet layer left its waker parked on the
/// host's change signal until the next write anywhere on the host: one per timed-out long
/// poll, each holding its task alive. On a quiet host, a participant holding any feed grant
/// could grow the host's memory without bound by repeating short waits, and an honest
/// follower's minute-long polls piled up the same way.
///
/// Each read runs on its own thread, so each is its own task with its own waker, as each
/// request is on a door (one task polling several reads shares one waker, which the signal
/// already kept once).
#[test]
fn a_long_poll_that_times_out_leaves_nothing_parked() {
    let f = fixture();
    let bobby = member(&participant("bobby"));
    let reads = [
        ("urn:layer:talk:feed", "0"),
        ("urn:layer:talk-bobby:feed", "0"),
        ("urn:layers:feed", "talk:0,talk-bobby:0"),
    ];
    std::thread::scope(|scope| {
        for i in 0..24 {
            let (iri, after) = reads[i % reads.len()];
            let (kernel, bobby) = (&f.kernel, &bobby);
            scope.spawn(move || {
                let answer = issue(
                    kernel,
                    Verb::Source,
                    iri,
                    &[("after", after), ("wait", "1")],
                    bobby,
                )
                .unwrap_or_else(|e| panic!("{iri}: {e}"));
                assert_eq!(text(&answer), "", "{iri} is quiet");
            });
        }
    });
    assert_eq!(
        f.host.notify().parked(),
        0,
        "every wait has ended, so nothing should be parked on the change signal"
    );
}

/// JSON-LD nested `depth` objects deep, `{"a:b":{"a:b":…1…}}`: 8 bytes a level, so 1500
/// levels is 12 KB, under the default 16 KiB `entry_max`.
fn nested_json_ld(depth: usize) -> String {
    let mut body = "{\"a:b\":".repeat(depth);
    body.push('1');
    body.push_str(&"}".repeat(depth));
    body
}

/// The environment variable that turns [`depth_probe_child`] on and names its write.
const DEPTH_PROBE: &str = "IKIGAI_LAYERS_DEPTH_PROBE";

/// The child's half of the depth test: inert unless a parent named a write (`say` or `out`).
/// It makes one deeply nested write on its own 2 MiB thread, as on a door, prints the outcome
/// on one line, and exits before the harness can.
#[test]
fn depth_probe_child() {
    let Ok(write) = std::env::var(DEPTH_PROBE) else {
        return;
    };
    let f = fixture();
    let body = nested_json_ld(1500);
    assert!(body.len() < 16 * 1024, "{} bytes", body.len());
    let (verb, iri, cap) = match write.as_str() {
        "say" => (
            Verb::Sink,
            "urn:layer:talk:say",
            member(&participant("bobby")),
        ),
        "out" => (Verb::Sink, TUPLES, tasker(&gonk())),
        other => panic!("no write named {other}"),
    };
    let outcome = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn_scoped(scope, || {
                issue(&f.kernel, verb, iri, &[("content", &body)], &cap)
            })
            .expect("a thread")
            .join()
            .expect("the write returns rather than panicking")
    });
    let outcome = match outcome {
        Err(ikigai_core::Error::InvalidArgument { name, detail }) => {
            format!("refused {name}: {detail}")
        }
        other => format!("not refused: {other:?}"),
    };
    println!("\nOUTCOME {}", outcome.replace('\n', " "));
    // `exit` runs no destructors: remove the scratch directories first.
    drop(f);
    std::process::exit(0);
}

/// The parent's half: run one write in a child process and return the outcome it printed, or
/// fail naming how the child died. An abort kills only the child, so it reads here as one
/// failed test rather than the whole test binary going down.
fn depth_probe(write: &str) -> String {
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "depth_probe_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(DEPTH_PROBE, write)
        .output()
        .expect("the probe child runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "a `{write}` of JSON-LD nested 1500 deep did not survive a 2 MiB thread ({}): {}",
        out.status,
        stderr
            .lines()
            .find(|l| l.contains("overflow"))
            .unwrap_or(&stderr)
    );
    let at = stdout
        .find("\nOUTCOME ")
        .unwrap_or_else(|| panic!("the `{write}` probe reported nothing: {stdout}"));
    stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

/// JSON-LD's parser recurses once per nested object, so one `say` (or `out`) of deeply nested
/// JSON-LD under the layer's own size bound overflowed the stack of the thread serving it,
/// and a stack overflow ABORTS the process: one participant holding `say` on any layer could
/// stop the host for everyone. Measured on a 2 MiB thread (the door's runtime gives its
/// workers 2 MiB): 1500 levels, 12 KB, aborted a release build. Each deep write runs in a
/// CHILD PROCESS (this test binary re-executed with [`DEPTH_PROBE`] set) on its own 2 MiB
/// thread, as on a door, because an abort cannot be caught in-process: a regression fails this
/// one test with the signal named, and the rest of the suite still reports (ledger #1728).
#[test]
fn deeply_nested_json_ld_is_refused_not_a_stack_overflow() {
    let said = depth_probe("say");
    assert!(
        said.starts_with("refused content: ") && said.contains("nested"),
        "a refusal on content, not {said}"
    );
    let dropped = depth_probe("out");
    assert!(
        dropped.starts_with("refused content: "),
        "a refusal on content, not {dropped}"
    );
    // A JSON-LD entry nested as deep as real entries are still parses: the bound refuses
    // only depth no entry needs.
    let f = fixture();
    let ok = format!(
        r#"{{"@context": {{"ik": "{IK}", "body": {{"@id": "ik:body"}}}},
            "@id": "urn:layer:new", "@type": "ik:Message", "body": [{{"@value": "nested [{{x}}] \"ok\""}}]}}"#
    );
    say(&f.kernel, "talk", &participant("bobby"), &ok).expect("an ordinary JSON-LD entry");
}

/// Ledger #1307's rule, applied here: a name joined to a path refuses a Windows device name
/// and a drive prefix. A layer's name is joined to the state directory, a participant's short
/// name to its layer's `graphs/`, and a definition's shapes and context file names to
/// `layers/`. On Windows `<state>\nul` is the null device, not a directory, and
/// `Path::join("C:x")` replaces the base, so a definition naming `C:…` reads a file outside
/// the configuration. Neither behavior can be observed on this platform: what these tests
/// observed before the fix is that each name was ACCEPTED and reached the disk.
#[test]
fn a_device_name_is_no_layer_or_participant_name() {
    for name in ["nul", "con", "aux", "prn", "com1", "lpt9", "com0"] {
        assert!(
            !ikigai_layers::name::is_layer_name(name),
            "`{name}` is a Windows device name, refused as a layer name"
        );
        assert!(
            ikigai_layers::name::Participant::parse(&participant(name)).is_err(),
            "`{name}` is a Windows device name, refused as a participant"
        );
    }
    // Only the whole word: a name that merely begins with one is a name.
    for name in ["nullable", "console", "com10", "auxiliary", "lpt"] {
        assert!(ikigai_layers::name::is_layer_name(name), "`{name}`");
    }
    let config = Scratch::new("device-layer");
    write_config(&config);
    config.write(
        "layers/nul.toml",
        "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\n",
    );
    let state = Scratch::new("device-state");
    let refused = ikigai_layers::Host::open(config.path(), state.path());
    assert!(refused.is_err(), "a layer named `nul` opened");
    assert!(!state.path().join("nul").exists(), "`nul` reached the disk");
}

#[test]
fn a_definition_names_no_drive_prefixed_or_device_file() {
    for shapes in [
        "C:msg.shapes.ttl",
        "nul",
        "con.ttl",
        "msg.shapes.ttl.",
        "msg.shapes.ttl ",
    ] {
        let config = Scratch::new("drive");
        write_config(&config);
        config.write("layers/C:msg.shapes.ttl", SHAPES);
        config.write("layers/con.ttl", SHAPES);
        config.write(
            "layers/notes.toml",
            &format!(
                "model = \"channel\"\nshapes = \"{shapes}\"\ncontext = \"msg.context.jsonld\"\n"
            ),
        );
        match ikigai_layers::Config::load(config.path()) {
            Ok(_) => panic!("a definition naming shapes `{shapes}` loaded"),
            Err(e) => assert!(
                e.to_string().contains("must be a file name"),
                "{shapes}: {e}"
            ),
        }
    }
}
