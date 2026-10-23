//! `ikigai-layers onboard setup` (ledger #2125), driven as the binary with no terminal: stdin is a
//! pipe or nothing, in a hermetic config home and door directory.

#![cfg(feature = "host")]

mod common;

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::*;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::spki::EncodePublicKey;
use ed25519_dalek::SigningKey;
use ikigai_layers::onboard::setup::LAYER_FILES;
use ikigai_layers::Host;

fn public_pem(seed: u8) -> String {
    SigningKey::from_bytes(&[seed; 32])
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("an SPKI PEM")
}

/// What `ikigai-gonk onboard init` prints for the edge: the table, with trust_dir commented
/// out, and the PEM.
fn gonk_block(controller: &str, pem: &str) -> String {
    format!(
        "the layer host's [onboard] table, for its layers.toml:\n\n[onboard]\n\
         grants         = \"grants\"\ncontroller     = \"{controller}\"\n\
         controller_key = \"controller.pub.pem\"\n\
         # trust_dir    = \"<the door's quic cert dir>/clients\"   # SET THIS\n\
         trust_reload   = \"live\"\n\nand controller.pub.pem beside layers.toml:\n\n{pem}"
    )
}

/// A door's QUIC certificate directory, as cli's `serve quic://` keeps it.
fn door_dir(label: &str) -> Scratch {
    let door = Scratch::new(label);
    door.write("server.crt", "not read by setup\n");
    door.write("server.key", "not read by setup\n");
    door
}

/// The common fixture (participants, brian among them, manifests and their layers).
fn config() -> Scratch {
    let config = Scratch::new("setup-cfg");
    write_config(&config);
    config
}

fn run(args: &[&str], stdin: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ikigai-layers"))
        .args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the binary runs");
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn setup(config: &Scratch, door: &Scratch, stdin: &str) -> Output {
    run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            "edge",
            "--controller-key",
            "-",
        ],
        Some(stdin),
    )
}

fn check(config: &Scratch, door: &Scratch, name: &str) -> Output {
    run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            name,
            "--check",
        ],
        None,
    )
}

/// Every file under `dir`, with its bytes.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut all = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                all.push((
                    path.strip_prefix(dir).unwrap().display().to_string(),
                    std::fs::read(&path).unwrap(),
                ));
            }
        }
    }
    all.sort();
    all
}

#[test]
fn setup_writes_what_a_real_host_opens_on_and_a_rerun_writes_nothing() {
    let config = config();
    config.write("layers.toml", "[match]\nbudget_ms = 2000\n");
    let door = door_dir("setup-door");
    let pem = public_pem(0x11);

    let first = setup(&config, &door, &gonk_block("brian", &pem));
    let said = out(&first);
    assert!(first.status.success(), "{said}");
    for (rel, text) in LAYER_FILES {
        assert!(said.contains(&format!("wrote     {rel}")), "{said}");
        assert_eq!(
            std::fs::read_to_string(config.path().join(rel)).unwrap(),
            text
        );
    }
    assert!(said.contains("wrote     collabs/onboard.toml"), "{said}");
    assert!(said.contains("restart the door"), "{said}");
    assert!(
        said.contains("checked: a node approved here leaves the lobby"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(config.path().join("controller.pub.pem")).unwrap(),
        pem
    );
    // The backup holds layers.toml as it was; the table is appended after what was there.
    let backups: Vec<_> = std::fs::read_dir(config.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("layers.toml.") && n.ends_with(".bak"))
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    assert_eq!(
        std::fs::read_to_string(config.path().join(&backups[0])).unwrap(),
        "[match]\nbudget_ms = 2000\n"
    );
    let host_toml = std::fs::read_to_string(config.path().join("layers.toml")).unwrap();
    assert!(
        host_toml.starts_with("[match]\nbudget_ms = 2000\n"),
        "{host_toml}"
    );

    // A real host opens on it, with a lobby, and trusts certificates where the door reads them.
    let state = Scratch::new("setup-state");
    let host = Host::open(config.path(), state.path(), &key(config.path())).expect("the host");
    assert_eq!(host.lobbies(), vec!["enroll"]);
    let onboard = host.onboard_config().expect("an [onboard] table");
    assert_eq!(onboard.controller, participant("brian"));
    assert_eq!(onboard.trust_dir, door.path().join("clients"));
    assert!(host
        .lobby_capability("edge", &"ab".repeat(32))
        .unwrap()
        .is_some());
    drop(host);

    // Idempotent: the same command again writes nothing, and says so.
    let before = snapshot(config.path());
    let again = setup(&config, &door, &gonk_block("brian", &pem));
    let said = out(&again);
    assert!(again.status.success(), "{said}");
    assert!(
        said.contains("nothing written: everything is in place"),
        "{said}"
    );
    assert!(said.contains("no restart"), "{said}");
    assert!(!said.contains("wrote "), "{said}");
    assert_eq!(before, snapshot(config.path()));
}

#[test]
fn a_file_that_differs_is_kept_and_an_existing_table_is_compared_not_rewritten() {
    let config = config();
    let door = door_dir("setup-door");
    let other_door = door_dir("setup-door-b");
    let pem = public_pem(0x11);
    config.write("layers/enroll.toml", "model = \"channel\"\nlabel = \"Mine\"\nshapes = \"enroll.shapes.ttl\"\ncontext = \"enroll.context.jsonld\"\nlobby = true\n");
    config.write("controller.pub.pem", &pem);
    let table = format!(
        "[onboard]\ngrants = \"grants\"\ncontroller = \"brian\"\ncontroller_key = \"controller.pub.pem\"\ntrust_dir = \"{}\"\n",
        p(&other_door.path().join("clients"))
    );
    config.write("layers.toml", &table);

    let o = setup(&config, &door, &pem);
    let said = out(&o);
    assert!(
        said.contains("kept      layers/enroll.toml: it differs from the embedded copy, first difference at line"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(config.path().join("layers/enroll.toml"))
            .unwrap()
            .lines()
            .nth(1),
        Some("label = \"Mine\"")
    );
    assert!(
        said.contains("differs   layers.toml [onboard], kept as it is: trust_dir is"),
        "{said}"
    );
    assert_eq!(
        std::fs::read_to_string(config.path().join("layers.toml")).unwrap(),
        table,
        "an existing table is never rewritten"
    );
    // The rest was written, and the check after it names what still leaves a node stuck.
    assert!(said.contains("wrote     layers/grants.toml"), "{said}");
    assert!(said.contains("stuck: trust_dir is"), "{said}");
    assert!(!o.status.success());
}

#[test]
fn setup_refuses_without_writing_anything() {
    let config = config();
    let door = door_dir("setup-door");
    let pem = public_pem(0x11);
    let before = snapshot(config.path());

    // No --door-dir: refused with the exact flag, never a guess.
    let o = run(
        &["onboard", "setup", p(config.path()), "--door-name", "edge"],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(out(&o).contains("pass --door-dir <the door's QUIC certificate directory>"));

    // A directory that is not a door's.
    let not_a_door = Scratch::new("setup-notdoor");
    let o = run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(not_a_door.path()),
            "--door-name",
            "edge",
            "--controller-key",
            "-",
        ],
        Some(&pem),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        out(&o).contains("holds no server.crt or server.key"),
        "{}",
        out(&o)
    );

    // A name the lobby would refuse.
    let o = run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            "Edge-Door",
            "--controller-key",
            "-",
        ],
        Some(&pem),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        out(&o).contains("--door-name: `Edge-Door` is not a host name"),
        "{}",
        out(&o)
    );

    // No key, and stdin is nothing at all.
    let o = run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            "edge",
        ],
        None,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("no controller"), "{}", out(&o));

    // A controller who is no participant: the staged configuration does not load.
    let o = run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            "edge",
            "--controller",
            "nobody",
            "--controller-key",
            "-",
        ],
        Some(&pem),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("nothing written"), "{}", out(&o));

    // --controller disagreeing with gonk's block.
    let o = run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            "edge",
            "--controller",
            "chris",
            "--controller-key",
            "-",
        ],
        Some(&gonk_block("brian", &pem)),
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("give one"), "{}", out(&o));

    assert_eq!(before, snapshot(config.path()));
}

#[test]
fn the_manifest_names_the_controller_given() {
    let config = config();
    let door = door_dir("setup-door");
    let o = run(
        &[
            "onboard",
            "setup",
            p(config.path()),
            "--door-dir",
            p(door.path()),
            "--door-name",
            "edge",
            "--controller",
            "chris",
            "--controller-key",
            "-",
        ],
        Some(&public_pem(0x11)),
    );
    assert!(o.status.success(), "{}", out(&o));
    let manifest = std::fs::read_to_string(config.path().join("collabs/onboard.toml")).unwrap();
    assert!(
        manifest.contains("\nchris = [\"read\", \"feed\", \"say\"]"),
        "{manifest}"
    );
    assert!(!manifest.contains("\nbrian = "), "{manifest}");
    let state = Scratch::new("setup-state");
    let host = Host::open(config.path(), state.path(), &key(config.path())).expect("the host");
    assert_eq!(
        host.onboard_config().unwrap().controller,
        participant("chris")
    );
}

/// A configuration setup completed, for `--check` to break one way at a time.
fn completed() -> (Scratch, Scratch) {
    let config = config();
    let door = door_dir("setup-door");
    let o = setup(&config, &door, &gonk_block("brian", &public_pem(0x11)));
    assert!(o.status.success(), "{}", out(&o));
    let c = check(&config, &door, "edge");
    assert!(c.status.success(), "{}", out(&c));
    (config, door)
}

fn assert_stuck(config: &Scratch, door: &Scratch, name: &str, wanted: &str) {
    let before = snapshot(config.path());
    let o = check(config, door, name);
    let said = out(&o);
    assert_eq!(o.status.code(), Some(1), "{said}");
    assert!(said.contains(wanted), "wanted {wanted:?} in:\n{said}");
    assert!(said.contains("nothing written (--check)"), "{said}");
    assert_eq!(before, snapshot(config.path()), "--check is read-only");
}

#[test]
fn check_finds_no_lobby() {
    let (config, door) = completed();
    // The lobby's definition without `lobby = true`.
    let text = std::fs::read_to_string(config.path().join("layers/enroll.toml")).unwrap();
    config.write("layers/enroll.toml", &text.replace("lobby   = true", ""));
    assert_stuck(&config, &door, "edge", "no lobby layer");
}

#[test]
fn check_finds_a_missing_or_unreadable_controller_key() {
    let (config, door) = completed();
    config.remove("controller.pub.pem");
    assert_stuck(&config, &door, "edge", "the [onboard] table does not load");
    config.write("controller.pub.pem", "not a key\n");
    assert_stuck(
        &config,
        &door,
        "edge",
        "`onboard.controller_key` `controller.pub.pem`",
    );
}

#[test]
fn check_finds_a_trust_dir_the_door_does_not_read() {
    let (config, _) = completed();
    let other = door_dir("setup-door-b");
    assert_stuck(&config, &other, "edge", "not the door's client directory");
    let not_a_door = Scratch::new("setup-notdoor");
    assert_stuck(
        &config,
        &not_a_door,
        "edge",
        "holds no server.crt or server.key",
    );
}

#[test]
fn check_finds_a_bad_door_name() {
    let (config, door) = completed();
    assert_stuck(&config, &door, "Edge", "`Edge` is not a host name");
    assert_stuck(&config, &door, &"a".repeat(49), "is not a host name");
}

#[test]
fn check_finds_a_stale_enrollment_pair() {
    let (config, door) = completed();
    config.write("grants.json", "{}\n");
    assert_stuck(
        &config,
        &door,
        "edge",
        "grants.json and clients.json are not what",
    );
    let (config, door) = completed();
    config.remove("clients.json");
    assert_stuck(
        &config,
        &door,
        "edge",
        "grants.json and clients.json are not what",
    );
}

#[test]
fn check_finds_a_manifest_granting_say_on_grants_to_another() {
    let (config, door) = completed();
    let manifest = std::fs::read_to_string(config.path().join("collabs/onboard.toml")).unwrap();
    config.write(
        "collabs/onboard.toml",
        &manifest.replace(
            "brian = [\"read\", \"feed\", \"say\"]",
            "brian = [\"read\", \"feed\", \"say\"]\nchris = [\"say\"]",
        ),
    );
    assert_stuck(
        &config,
        &door,
        "edge",
        "grants `say` on `grants` to urn:collab:participant:chris",
    );
}

#[test]
fn check_on_a_bare_config_names_every_missing_piece() {
    let config = config();
    let door = door_dir("setup-door");
    let o = check(&config, &door, "edge");
    let said = out(&o);
    assert_eq!(o.status.code(), Some(1), "{said}");
    assert!(said.contains("no lobby layer"), "{said}");
    assert!(said.contains("has no [onboard] table"), "{said}");
    assert!(
        said.contains("grants.json and clients.json are not what"),
        "{said}"
    );
}
