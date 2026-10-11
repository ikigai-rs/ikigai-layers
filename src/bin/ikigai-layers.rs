//! `ikigai-layers grants <config dir> [--out <dir>] [--check]`: derive a layer door's
//! `grants.json` and `clients.json` from the manifests.

use std::path::PathBuf;
use std::process::ExitCode;

use ikigai_layers::grants::{generate, load_fingerprints};
use ikigai_layers::Config;

const USAGE: &str = "usage: ikigai-layers grants <config dir> [--out <dir>] [--check]

Derives the layer door's enrollment from <config dir>'s manifests (collabs/*.toml),
participants (participants.ttl) and certificate fingerprints (enrollment.toml), and writes
grants.json and clients.json into --out (default: <config dir>), each atomically.
--check verifies the configuration and prints what would be written, writing nothing.
A manifest that mounts another collaboration's layer is refused and nothing is written.";

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

fn run(args: &[String]) -> Result<(), (u8, String)> {
    let usage = || (2, USAGE.to_string());
    let mut args = args.iter();
    if args.next().map(String::as_str) != Some("grants") {
        return Err(usage());
    }
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
    let config = Config::load(&dir).map_err(|e| (1, e.to_string()))?;
    let fingerprints = load_fingerprints(&dir, &config).map_err(|e| (1, e.to_string()))?;
    let enrollment = generate(&config, &fingerprints).map_err(|e| (1, e.to_string()))?;
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
