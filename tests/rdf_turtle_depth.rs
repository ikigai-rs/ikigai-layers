//! The participant and gateway Turtle reader must bound triple-term nesting before parsing.
use ikigai_layers::rdf::parse_turtle;

const PROBE: &str = "IKIGAI_LAYERS_RDF_TURTLE_PROBE";

fn nested(shape: &str, depth: usize) -> String {
    let (open, inner, close) = match shape {
        "triple" => ("<<(: a", " :", ")>>"),
        "reified" => ("<< : a ", ":", " >>"),
        "bnode" => ("[: ", ":", "]"),
        "collection" => ("(", "", ")"),
        _ => panic!("unknown shape"),
    };
    format!(
        "@prefix : <urn:x:> . :s :p {}{inner}{} .",
        open.repeat(depth),
        close.repeat(depth)
    )
}

#[test]
fn parse_turtle_probe_child() {
    let Ok(shape) = std::env::var(PROBE) else {
        return;
    };
    // An overflow aborts only this child. Exercise parsing AND dropping its result on the
    // same 2 MiB stack a door worker uses, including RDF 1.2 via the test dependency.
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(move || {
            // Structural shapes were measured not to recurse at 32,000 levels in
            // ikigai-linkeddata/tests/structural_depth.rs; they need no depth refusal.
            if matches!(shape.as_str(), "bnode" | "collection") {
                parse_turtle(&nested(&shape, 32_000)).expect("iterative shapes parse safely");
                return;
            }
            let error = parse_turtle(&nested(&shape, 10_000))
                .expect_err("deep Turtle must be refused before parsing");
            assert!(error.contains("Turtle nested 10000 levels deep"), "{error}");
            assert!(error.contains("over the bound of 64"), "{error}");
        })
        .unwrap()
        .join()
        .expect("the parser returns without panicking");
}

#[test]
fn deep_turtle_is_checked_in_a_child_process() {
    for shape in ["triple", "reified", "bnode", "collection"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "parse_turtle_probe_child", "--nocapture"])
            .env(PROBE, shape)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{shape}: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn depth_boundary_and_brackets_in_text() {
    for shape in ["triple", "reified", "bnode", "collection"] {
        parse_turtle(&nested(shape, 64)).expect("64 levels are allowed");
        // Only triple terms recurse; the 32,000-level structural measurement above
        // justifies accepting blank-node lists and collections beyond 64.
        if matches!(shape, "bnode" | "collection") {
            parse_turtle(&nested(shape, 65)).expect("structural depth is not bounded");
            continue;
        }
        let error = parse_turtle(&nested(shape, 65)).expect_err("65 levels are refused");
        assert!(error.contains("Turtle nested 65 levels deep"), "{error}");
    }
    let brackets = "[()<<({|".repeat(100);
    let text = format!("<urn:s(> <urn:p> \"{brackets}\", '''{brackets}''' . # {brackets}\n");
    parse_turtle(&text).expect("brackets in IRIs, strings and comments are not nesting");
}

// ikigai-linkeddata measured these iterative parser shapes at 32,000 levels.
#[test]
fn iterative_shapes_are_not_subject_to_the_triple_term_bound() {
    for shape in ["bnode", "collection"] {
        parse_turtle(&nested(shape, 65)).expect("iterative nesting is safe to parse");
    }
}
