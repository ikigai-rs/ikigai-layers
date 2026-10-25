//! The participant build reads no platform time outside its native-only module
//! (ledger #2193).
//!
//! On wasm32-unknown-unknown, `Instant::now()`, `SystemTime::now()`, `process::id()`, a
//! spawned thread and futures-timer's native `Delay` all COMPILE and then fail at runtime.
//! So CI's wasm32 clippy of the participant (`cargo clippy --lib --target
//! wasm32-unknown-unknown --no-default-features`) cannot see one of them creep back into
//! the gateway: it would build green and panic on the first `ask` in a browser. This guard
//! is the check that can. A participant's time is the `Clock` its asker was given
//! (`Asker::with_clock`) and its waits are the `Timer` (`Asker::with_timer`); the platform
//! is read only in `native` (the native defaults), declared behind
//! `cfg(not(target_arch = "wasm32"))` in `lib.rs`.
//!
//! Only the modules a participant links are held to this: the host's (`#[cfg(feature =
//! "host")]` in `lib.rs`) are files under a state directory and native by design. Comment
//! lines are skipped, so prose and doctests (which run natively) may name these; the
//! `#[cfg(test)]` module at the end of a file is skipped for the same reason.

use std::path::Path;

/// The modules a participant (`default-features = false`) links, which must build and RUN
/// on wasm32. `onboard/mod.rs`'s host items are gated inside it, and it reads no time at all.
const PARTICIPANT: [&str; 8] = [
    "gateway.rs",
    "id.rs",
    "lib.rs",
    "name.rs",
    "onboard/mod.rs",
    "rdf.rs",
    "time.rs",
    "timer.rs",
];

/// The module that may read the platform, declared native-only in `lib.rs`.
const NATIVE_ONLY: &str = "native";

/// What compiles on wasm32 and fails there at runtime.
const PLATFORM: [&str; 6] = [
    "Instant",
    "SystemTime",
    "futures_timer",
    "process::id",
    "thread::spawn",
    "thread::sleep",
];

const NATIVE_CFG: &str = "#[cfg(not(target_arch = \"wasm32\"))]";

fn src() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The file's code lines before its `#[cfg(test)]` module, comments dropped.
fn code(text: &str) -> Vec<(usize, &str)> {
    text.lines()
        .enumerate()
        .take_while(|(_, line)| line.trim() != "#[cfg(test)]")
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .map(|(n, line)| (n + 1, line))
        .collect()
}

#[test]
fn the_participant_reads_no_platform_time() {
    let mut offenders = Vec::new();
    for file in PARTICIPANT {
        let text = std::fs::read_to_string(src().join(file)).expect("a participant module");
        for (n, line) in code(&text) {
            for token in PLATFORM {
                if line.contains(token) {
                    offenders.push(format!("src/{file}:{n}: `{token}` in `{}`", line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "platform time read in a participant module; take it from the asker's Clock and \
         Timer instead (or put the native default in `src/{NATIVE_ONLY}.rs`):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_native_module_is_left_out_of_a_wasm32_build() {
    let lib = std::fs::read_to_string(src().join("lib.rs")).expect("lib.rs");
    let lines = code(&lib);
    let declaration = format!("mod {NATIVE_ONLY};");
    let at = lines
        .iter()
        .position(|(_, line)| line.trim() == declaration)
        .unwrap_or_else(|| panic!("lib.rs declares no `{declaration}`"));
    assert!(
        at > 0 && lines[at - 1].1.trim() == NATIVE_CFG,
        "`{declaration}` must sit directly under `{NATIVE_CFG}`, or wasm32 compiles it"
    );
}

/// futures-timer is linked on native targets only: on wasm32 its native backend is a
/// helper thread that cannot be spawned.
#[test]
fn futures_timer_is_a_native_dependency() {
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("Cargo.toml");
    let mut section = "";
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line;
        } else if line.starts_with("futures-timer") {
            assert_eq!(
                section, "[target.'cfg(not(target_arch = \"wasm32\"))'.dependencies]",
                "futures-timer must be a native-only dependency"
            );
        }
    }
}
