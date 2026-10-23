//! `ikigai-layers grants <config dir> [--out <dir>] [--check]`: derive a layer door's
//! `grants.json` and `clients.json` from the manifests.
//!
//! `ikigai-layers onboard setup <config dir> --door-dir <dir> --door-name <name> …`: the layer
//! host's half of onboarding in one command (ledger #2125), and `--check`, its read-only half.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use ikigai_layers::grants::derive;
use ikigai_layers::onboard::setup::{self, Options};

const USAGE: &str = "usage: ikigai-layers grants <config dir> [--out <dir>] [--check]
       ikigai-layers onboard setup <config dir> --door-dir <dir> --door-name <name>
                     [--controller <participant>] [--controller-key <file>|-] [--check]

grants derives the layer door's enrollment from <config dir>'s manifests (collabs/*.toml),
participants (participants.ttl) and certificate fingerprints (enrollment.toml), and writes
grants.json and clients.json into --out (default: <config dir>), each atomically.
--check verifies the configuration and prints what would be written, writing nothing.
A manifest that mounts another collaboration's layer is refused and nothing is written.

onboard setup writes the layer host's half of onboarding into <config dir>: the lobby and
grants layers and the onboard manifest (embedded copies; a file that differs is kept), the
[onboard] table appended to layers.toml with a backup (an existing table is compared, never
rewritten), the controller key beside it, and grants.json and clients.json.
  --door-dir        the door's QUIC certificate directory (it holds server.crt and server.key);
                    trust_dir is its clients/
  --door-name       the door's --name; refused when the lobby would refuse every stranger
  --controller      the participant who approves nodes (default: the one gonk's block names)
  --controller-key  the controller's public key: a PEM file, or - for stdin; the whole output
                    of `ikigai-gonk onboard init` may be piped in
  --check           read-only: report every way a node would be left in the lobby, and exit
                    non-zero on any";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, message)) => {
            eprintln!("{message}");
            ExitCode::from(code)
        }
    }
}

type Outcome = Result<(), (u8, String)>;

fn usage() -> (u8, String) {
    (2, USAGE.to_string())
}

fn run(args: &[String]) -> Outcome {
    match args.first().map(String::as_str) {
        Some("grants") => grants(&args[1..]),
        Some("onboard") if args.get(1).map(String::as_str) == Some("setup") => {
            onboard_setup(&args[2..])
        }
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn grants(args: &[String]) -> Outcome {
    let mut args = args.iter();
    let mut dir = None;
    let mut out = None;
    let mut check = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = Some(PathBuf::from(args.next().ok_or_else(usage)?)),
            "--check" => check = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            flag if flag.starts_with('-') => return Err(usage()),
            path if dir.is_none() => dir = Some(PathBuf::from(path)),
            _ => return Err(usage()),
        }
    }
    let dir = dir.ok_or_else(usage)?;
    let (_, enrollment) = derive(&dir).map_err(|e| (1, e.to_string()))?;
    for (grant, scopes) in &enrollment.grants {
        println!("grant {grant}: {}", scopes.join(" "));
    }
    for (fp, client) in &enrollment.clients {
        println!("client {fp} -> {} ({})", client.grant, client.principal);
    }
    for iri in &enrollment.unenrolled {
        println!("unenrolled {iri}: granted, but no certificate fingerprint yet");
    }
    for iri in &enrollment.idle {
        println!("idle {iri}: a certificate, but no manifest grants it anything");
    }
    if check {
        println!("checked; nothing written (--check)");
        return Ok(());
    }
    let out = out.unwrap_or(dir);
    enrollment.write(&out).map_err(|e| (1, e))?;
    println!(
        "wrote {} and {}",
        out.join("grants.json").display(),
        out.join("clients.json").display()
    );
    Ok(())
}

fn onboard_setup(args: &[String]) -> Outcome {
    let mut args = args.iter();
    let mut dir = None;
    let mut door_dir = None;
    let mut door_name = None;
    let mut controller = None;
    let mut key = None;
    let mut check = false;
    let value = |args: &mut std::slice::Iter<'_, String>, flag: &str| {
        args.next()
            .cloned()
            .ok_or_else(|| (2, format!("{flag} takes a value\n\n{USAGE}")))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--door-dir" => door_dir = Some(PathBuf::from(value(&mut args, arg)?)),
            "--door-name" => door_name = Some(value(&mut args, arg)?),
            "--controller" => controller = Some(value(&mut args, arg)?),
            "--controller-key" => key = Some(value(&mut args, arg)?),
            "--check" => check = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            flag if flag.starts_with('-') => {
                return Err((2, format!("unknown flag {flag}\n\n{USAGE}")))
            }
            path if dir.is_none() => dir = Some(PathBuf::from(path)),
            _ => return Err(usage()),
        }
    }
    let dir = dir.ok_or_else(usage)?;
    let door_dir = door_dir.ok_or_else(|| {
        (
            2,
            "pass --door-dir <the door's QUIC certificate directory>: the directory cli's \
             `serve quic://` keeps server.crt, server.key and clients/ in (on the edge, \
             /home/ikigai/.ikigai/quic-layers/edge-bundle). trust_dir is its clients/, and it is \
             never guessed: with a wrong one a node waits in the lobby forever"
                .to_string(),
        )
    })?;
    let door_name = door_name.ok_or_else(|| {
        (
            2,
            "pass --door-name <name>: the door's --name, short and lowercase ([a-z0-9-]), \
             which the lobby principal carries"
                .to_string(),
        )
    })?;
    if check {
        if controller.is_some() || key.is_some() {
            return Err((
                2,
                "--check reads the configuration as it is: --controller and --controller-key \
                 are setup's"
                    .to_string(),
            ));
        }
        let problems = setup::check(&dir, &door_dir, &door_name);
        if problems.is_empty() {
            println!("checked: a node approved here leaves the lobby (nothing written)");
            return Ok(());
        }
        for p in &problems {
            println!("stuck: {p}");
        }
        return Err((
            1,
            format!(
                "{} way(s) a node would be left in the lobby; nothing written (--check)",
                problems.len()
            ),
        ));
    }
    let key_input = match key.as_deref() {
        None => None,
        Some("-") => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| (1, format!("read the controller key from stdin: {e}")))?;
            Some(text)
        }
        Some(file) => Some(
            std::fs::read_to_string(file)
                .map_err(|e| (1, format!("read the controller key {file}: {e}")))?,
        ),
    };
    let report = setup::setup(&Options {
        config_dir: dir.clone(),
        door_dir: door_dir.clone(),
        door_name: door_name.clone(),
        controller,
        key_input,
    })
    .map_err(|e| (1, e))?;
    for line in &report.lines {
        println!("{line}");
    }
    let problems = setup::check(&dir, &door_dir, &door_name);
    if problems.is_empty() {
        println!("checked: a node approved here leaves the lobby");
        return Ok(());
    }
    for p in &problems {
        println!("stuck: {p}");
    }
    Err((
        1,
        format!(
            "{} way(s) a node would still be left in the lobby",
            problems.len()
        ),
    ))
}
