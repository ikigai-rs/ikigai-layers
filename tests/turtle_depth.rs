//! Deeply nested TURTLE in a write (ledger #2015). JSON-LD's depth is bounded before it is
//! parsed (`MAX_JSON_DEPTH`, the audit of 2026-10-11); Turtle's shared bound covers recursive
//! triple terms. This suite also exercises the iterative structural shapes: RDF 1.2 triple terms `<<( … )>>`, reified triples `<< … >>`,
//! blank-node property lists `[ … ]` and collections `( … )`.
//!
//! Each deep write runs in a CHILD PROCESS (this test binary re-executed with [`PROBE`] set) on
//! its own 2 MiB thread, the stack a door's runtime gives each worker, because a stack overflow
//! aborts the process and cannot be caught in-process: a regression fails one test here with
//! the signal named, and the rest of the suite still reports.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use common::*;
use ikigai_core::Verb;

/// The environment variable that turns [`turtle_probe_child`] on: `{write}:{shape}:{depth}`.
const PROBE: &str = "IKIGAI_LAYERS_TURTLE_PROBE";

/// `depth` levels of one Turtle nesting shape, as the object of one triple. Each is the
/// tersest spelling of its shape (`:` is a whole prefixed name), so the most levels fit the
/// size bound.
fn nested(shape: &str, depth: usize) -> String {
    let (open, inner, close) = match shape {
        // An RDF 1.2 triple term whose object is a triple term: 9 bytes a level (`a<<(` lexes
        // as the keyword and then the opener).
        "triple" => ("<<(: a", " :", ")>>"),
        // A reified triple whose object is a reified triple: 10 bytes a level.
        "reified" => ("<< : a ", ":", " >>"),
        // A blank-node property list whose object is one: 4 bytes a level.
        "bnode" => ("[: ", ":", "]"),
        // A collection whose one member is a collection: 2 bytes a level.
        "collection" => ("(", "", ")"),
        other => panic!("no shape named {other}"),
    };
    format!("{}{inner}{}", open.repeat(depth), close.repeat(depth))
}

/// A `write` (`say` or `out`) whose one triple's object is nested `depth` levels deep.
fn body(write: &str, shape: &str, depth: usize) -> String {
    let subject = match write {
        "say" => "<urn:layer:new>".to_string(),
        "out" => format!("<{}>", offer_iri("deep")),
        other => panic!("no write named {other}"),
    };
    format!(
        "@prefix : <urn:x:> .\n{subject} a <{IK}Message> ; :p {} .\n",
        nested(shape, depth)
    )
}

/// The child's half: inert unless a parent named a probe. It opens the fixture with the talk
/// and task layers raised to the largest `entry_max` a layer may have (1 MiB), makes one write
/// on its own 2 MiB thread, as on a door, prints the outcome on one line, and exits before the
/// harness can.
#[test]
fn turtle_probe_child() {
    let Ok(probe) = std::env::var(PROBE) else {
        return;
    };
    let mut parts = probe.split(':');
    let (write, shape, depth) = (
        parts.next().expect("a write").to_string(),
        parts.next().expect("a shape").to_string(),
        parts
            .next()
            .expect("a depth")
            .parse::<usize>()
            .expect("a number"),
    );
    let mut f = fixture();
    f.config.write(
        "layers/talk.toml",
        "model = \"channel\"\nlabel = \"Talk\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\nshared = [\"bobby\", \"chris\"]\nentry_max = \"1MiB\"\n",
    );
    f.config.write(
        "layers/tasks-bobby.toml",
        "model = \"tuplespace\"\nlabel = \"Tasks for Bobby\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\naddressed = true\nentry_max = \"1MiB\"\n",
    );
    f.reopen();
    let content = body(&write, &shape, depth);
    let (iri, cap) = match write.as_str() {
        "say" => ("urn:layer:talk:say", member(&participant("bobby"))),
        _ => (TUPLES, tasker(&gonk())),
    };
    let outcome = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn_scoped(scope, || {
                issue(&f.kernel, Verb::Sink, iri, &[("content", &content)], &cap)
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
    println!(
        "\nOUTCOME {} bytes: {}",
        content.len(),
        outcome.replace('\n', " ")
    );
    // `exit` runs no destructors: remove the scratch directories first.
    drop(f);
    std::process::exit(0);
}

/// The parent's half: run one write in a child process. `Ok` is the outcome it printed; `Err`
/// names how the child died (an abort kills only the child).
fn probe(write: &str, shape: &str, depth: usize) -> Result<String, String> {
    let out = std::process::Command::new(std::env::current_exe().expect("this test binary"))
        .args([
            "--exact",
            "turtle_probe_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PROBE, format!("{write}:{shape}:{depth}"))
        .output()
        .expect("the probe child runs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!(
            "{}: {}",
            out.status,
            stderr
                .lines()
                .find(|l| l.contains("overflow"))
                .unwrap_or(&stderr)
        ));
    }
    let at = stdout
        .find("\nOUTCOME ")
        .ok_or_else(|| format!("the probe reported nothing: {stdout}"))?;
    Ok(stdout[at + "\nOUTCOME ".len()..]
        .lines()
        .next()
        .unwrap_or("")
        .to_string())
}

/// Triple terms nested 1,000, 10,000 and 100,000 deep, in a `say` and in an `out`: the parser
/// clones a triple term recursively when it emits the triple that holds it, and before the
/// bound 1,750 levels (16 KB, under the default `entry_max`) aborted a debug build's 2 MiB
/// thread and 4,500 levels (40 KB) a release build's. Each is refused on `content`, naming the
/// bound, before anything is parsed.
#[test]
fn deeply_nested_triple_terms_are_refused_not_a_stack_overflow() {
    for write in ["say", "out"] {
        for depth in [1_000, 10_000, 100_000] {
            let outcome = probe(write, "triple", depth).unwrap_or_else(|died| {
                panic!("a `{write}` of triple terms nested {depth} deep did not survive a 2 MiB thread ({died})")
            });
            assert!(
                outcome.contains("refused content: Turtle nested")
                    && outcome.contains("over the bound of 64"),
                "`{write}` {depth} deep: a refusal naming the bound, not {outcome}"
            );
        }
    }
}

/// Reified triples still share the triple-term bound. Blank-node lists and collections
/// were measured not to recurse at 32,000 levels (ikigai-linkeddata/tests/structural_depth.rs),
/// so those now reach the entry checks after parsing instead of a nesting refusal.
#[test]
fn deep_shapes_follow_the_shared_triple_term_rule() {
    for write in ["say", "out"] {
        for shape in ["reified", "bnode", "collection"] {
            let outcome = probe(write, shape, 100_000).unwrap_or_else(|died| {
                panic!("a `{write}` of {shape} nested 100,000 deep did not survive ({died})")
            });
            if matches!(shape, "bnode" | "collection") {
                assert!(!outcome.contains("Turtle nested"), "{outcome}");
                let admission = match write {
                    "say" => "more than one subject",
                    _ => "a blank-node subject",
                };
                assert!(outcome.contains(admission), "{outcome}");
                continue;
            }
            assert!(
                outcome.contains("refused content: Turtle nested 100000 levels deep"),
                "`{write}` {shape}: a refusal naming the depth, not {outcome}"
            );
        }
    }
}

/// The bound refuses only depth no entry needs: at 64 levels a write reaches the checks after
/// the parse (and is refused there, for what it says), at 65 it is refused on its nesting; an
/// ordinary entry whose TEXT is full of brackets is no nesting at all.
#[test]
fn the_bound_is_exact_and_ignores_brackets_in_text() {
    let f = fixture();
    let bobby = participant("bobby");
    let at = say(&f.kernel, "talk", &bobby, &body("say", "triple", 64))
        .expect_err("a quoted triple is refused after the parse");
    assert!(
        at.to_string().contains("quoted triple") && !at.to_string().contains("nested"),
        "64 levels parse: {at}"
    );
    // Blank-node lists at 65 are now allowed through the guard, as justified by the
    // 32,000-level structural measurement; use a recursive triple term for this boundary.
    let over = say(&f.kernel, "talk", &bobby, &body("say", "triple", 65))
        .expect_err("65 levels are over the bound");
    assert!(
        over.to_string().contains("Turtle nested 65 levels deep"),
        "{over}"
    );
    let text = format!(
        "@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; # [[(<<( in a comment\n\
         ik:body \"\"\"<<( [[ (( {{| in a long string\"\"\" .\n"
    );
    say(&f.kernel, "talk", &bobby, &text).expect("brackets in text are not nesting");
}

/// A survey, not a gate: every write, shape and depth, printed.
/// `cargo test --test turtle_depth survey -- --ignored --nocapture`.
#[test]
#[ignore]
fn survey() {
    for write in ["say", "out"] {
        for shape in ["triple", "reified", "bnode", "collection"] {
            for depth in [1_000, 10_000, 100_000] {
                println!("{write} {shape} {depth}: {:?}", probe(write, shape, depth));
            }
        }
    }
}
