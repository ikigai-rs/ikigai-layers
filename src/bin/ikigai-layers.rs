//! `ikigai-layers grants <config dir> [--out <dir>] [--check]`: derive a layer door's
//! `grants.json` and `clients.json` from the manifests.
//!
//! `ikigai-layers epoch <state dir> [--renew]`: print a state directory's feed epoch, or name a
//! new one after a restore (ledger #4114).
//!
//! `ikigai-layers onboard setup <config dir> --door-dir <dir> --door-name <name> …`: the layer
//! host's half of onboarding in one command (ledger #2125), and `--check`, its read-only half.

use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use ikigai_layers::grants::derive;
use ikigai_layers::onboard::join::descriptor;
use ikigai_layers::onboard::setup::{self, Options};

const USAGE: &str = "usage: ikigai-layers grants <config dir> [--out <dir>] [--check]
       ikigai-layers onboard setup <config dir> --door-dir <dir> --door-name <name>
                     [--controller <participant>] [--controller-key <file>|-] [--check]
       ikigai-layers descriptor <config dir> --door <quic://host:port> --door-dir <dir> --out <dir>
       ikigai-layers epoch <state dir> [--renew]

grants derives the layer door's enrollment from <config dir>'s manifests (collabs/*.toml),
participants (participants.ttl) and certificate fingerprints (enrollment.toml), and writes
grants.json and clients.json into --out (default: <config dir>), each atomically and 0600.
The participants layers.toml's join.count_readers names gain urn:cap:layers:read:join-count,
listed as count-reader lines; a name that is no participant, or a joined identity, is refused.
A <config dir> that is missing or holds no manifest is refused and nothing is written.
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
                    non-zero on any

descriptor writes layers.json and server.crt into --out, for a host to publish at
https://<host>/.well-known/ikigai/layers.json: the door's address, the certificate in --door-dir
and its sha256, the join lobby, and the public layers.

epoch prints the feed epoch of a layer host's <state dir>: the name of its feeds' incarnation,
which every feed's JSON face carries and a follower names back with epoch=. A restart keeps it,
and so does a backup. After restoring <state dir> from a backup, and before its host first
opens, run it with --renew: it names a new epoch, so a follower whose cursor came from the lost
events is refused and starts again from 0 instead of skipping events. --renew is refused while
a host holds <state dir>.";

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
        Some("descriptor") => write_descriptor(&args[1..]),
        Some("epoch") => epoch(&args[1..]),
        Some("-h" | "--help") => {
            println!("{USAGE}");
            Ok(())
        }
        _ => Err(usage()),
    }
}

fn epoch(args: &[String]) -> Outcome {
    let mut dir = None;
    let mut renew = false;
    for arg in args {
        match arg.as_str() {
            "--renew" => renew = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            flag if flag.starts_with('-') => {
                return Err((2, format!("unknown flag {flag}\n\n{USAGE}")))
            }
            path if dir.is_none() => dir = Some(PathBuf::from(path)),
            argument => {
                let message = format!(
                    "unexpected argument {argument}: epoch takes one state directory\n\n{USAGE}"
                );
                return Err((2, message));
            }
        }
    }
    let dir = dir.ok_or_else(usage)?;
    if renew {
        let epoch = ikigai_layers::Host::renew_epoch(&dir).map_err(|e| (1, e.to_string()))?;
        println!("epoch {epoch}: renewed; every follower starts again from 0");
        return Ok(());
    }
    match ikigai_layers::feed::read_epoch(&dir).map_err(|e| (1, e))? {
        Some(epoch) => println!("epoch {epoch}"),
        None => {
            return Err((
                1,
                format!(
                    "{} names no feed epoch yet: a layer host mints one when it next opens it",
                    dir.display()
                ),
            ))
        }
    }
    Ok(())
}

fn grants(args: &[String]) -> Outcome {
    let mut args = args.iter();
    let mut dir = None;
    let mut out = None;
    let mut check = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => {
                let value = args
                    .next()
                    .ok_or_else(|| (2, format!("--out takes a value\n\n{USAGE}")))?;
                if value.starts_with('-') {
                    return Err((
                        2,
                        format!("--out takes a value; got flag {value}\n\n{USAGE}"),
                    ));
                }
                out = Some(PathBuf::from(value));
            }
            "--check" => check = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            flag if flag.starts_with('-') => {
                return Err((2, format!("unknown flag {flag}\n\n{USAGE}")))
            }
            path if dir.is_none() => dir = Some(PathBuf::from(path)),
            argument => {
                let message = format!(
                    "unexpected argument {argument}: grants takes one config directory\n\n{USAGE}"
                );
                return Err((2, message));
            }
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
    for grant in &enrollment.count_readers {
        println!(
            "count-reader {grant}: gains urn:cap:layers:read:join-count, the join count's three \
             aggregate numbers (join.count_readers)"
        );
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

fn write_descriptor(args: &[String]) -> Outcome {
    let mut args = args.iter();
    let (mut dir, mut door, mut door_dir, mut out) = (None, None, None, None);
    let value = |args: &mut std::slice::Iter<'_, String>, flag: &str| {
        args.next()
            .cloned()
            .ok_or_else(|| (2, format!("{flag} takes a value\n\n{USAGE}")))
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--door" => door = Some(value(&mut args, arg)?),
            "--door-dir" => door_dir = Some(PathBuf::from(value(&mut args, arg)?)),
            "--out" => out = Some(PathBuf::from(value(&mut args, arg)?)),
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
    let missing = |flag: &str| (2, format!("pass {flag}\n\n{USAGE}"));
    let door = door.ok_or_else(|| missing("--door <quic://host:port>"))?;
    let door_dir =
        door_dir.ok_or_else(|| missing("--door-dir <the door's certificate directory>"))?;
    let out = out.ok_or_else(|| missing("--out <the directory to write into>"))?;
    let written = descriptor(&dir, &door, &door_dir).map_err(|e| (1, e))?;
    let (json, crt) = written.write(&out).map_err(|e| (1, e))?;
    println!("wrote {} and {}", json.display(), crt.display());
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
                "{} {} a node would be left in the lobby; nothing written (--check)",
                problems.len(),
                if problems.len() == 1 { "way" } else { "ways" }
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
            "{} {} a node would still be left in the lobby",
            problems.len(),
            if problems.len() == 1 { "way" } else { "ways" }
        ),
    ))
}
