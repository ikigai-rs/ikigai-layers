//! `ikigai-layers onboard setup` (ledger #2125): the layer host's half of onboarding in one
//! command, the counterpart of gonk's `onboard init`, so nobody copies or edits a file by hand.
//!
//! [`setup`] writes, into a host's config directory:
//!
//! - the lobby and grants layers (`layers/enroll.*`, `layers/grants.*`) and the manifest
//!   (`collabs/onboard.toml`), from the copies in `onboard/` of this repository, embedded in the
//!   binary. The manifest names the controller given. A file already there that differs is KEPT,
//!   and the report says where it differs;
//! - the `[onboard]` table, APPENDED to `layers.toml` with a backup beside it
//!   (`layers.toml.{unix seconds}.bak`), and the controller's public key beside it. A table
//!   already there is never rewritten: it is compared, and the differences reported;
//! - `trust_dir` as the door's own `clients/` directory, derived from `--door-dir`, which must
//!   be a door's QUIC certificate directory (it holds `server.crt` and `server.key`);
//! - `grants.json` and `clients.json`, by [`crate::grants::derive`], what `ikigai-layers grants`
//!   runs.
//!
//! Nothing is written unless the whole result is a configuration the host opens: every write is
//! first made in a staged copy of the configuration, which is loaded with the host's own
//! functions ([`Config::load`], [`load_onboard`], [`check_controller_says`]). A rerun with
//! everything in place writes nothing and says so.
//!
//! [`check`] is the read-only half: every way the configuration would leave a node waiting in
//! the lobby. Every rule it checks is the host's own, called, never restated here.

use std::path::{Path, PathBuf};

use crate::config::{check_controller_says, load_onboard, Config, HOST_FILE};
use crate::grants::derive;
use crate::name::Participant;
use crate::onboard::apply::write_atomic;
use crate::onboard::{check_public_key_pem, lobby_principal};

/// The embedded lobby and grants layers, by their path under the config directory.
pub const LAYER_FILES: [(&str, &str); 6] = [
    (
        "layers/enroll.toml",
        include_str!("../../onboard/layers/enroll.toml"),
    ),
    (
        "layers/enroll.shapes.ttl",
        include_str!("../../onboard/layers/enroll.shapes.ttl"),
    ),
    (
        "layers/enroll.context.jsonld",
        include_str!("../../onboard/layers/enroll.context.jsonld"),
    ),
    (
        "layers/grants.toml",
        include_str!("../../onboard/layers/grants.toml"),
    ),
    (
        "layers/grants.shapes.ttl",
        include_str!("../../onboard/layers/grants.shapes.ttl"),
    ),
    (
        "layers/grants.context.jsonld",
        include_str!("../../onboard/layers/grants.context.jsonld"),
    ),
];

/// Where the manifest goes.
pub const MANIFEST_FILE: &str = "collabs/onboard.toml";

/// The embedded manifest, naming [`EMBEDDED_CONTROLLER`].
const MANIFEST: &str = include_str!("../../onboard/collabs/onboard.toml");

/// The controller the embedded manifest names; [`manifest_text`] puts the real one in its place.
const EMBEDDED_CONTROLLER: &str = "brian";

/// The controller key's file, beside `layers.toml`, when setup writes the table.
pub const KEY_FILE: &str = "controller.pub.pem";

/// The grants layer the embedded files define.
pub const GRANTS_LAYER: &str = "grants";

/// The PEM labels of an SPKI public key.
const PEM_BEGIN: &str = "-----BEGIN PUBLIC KEY-----";
const PEM_END: &str = "-----END PUBLIC KEY-----";

/// What `setup` is asked to do.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The layer host's config directory.
    pub config_dir: PathBuf,
    /// The door's QUIC certificate directory (`--door-dir`).
    pub door_dir: PathBuf,
    /// The door's instance name (`--door-name`).
    pub door_name: String,
    /// The controller participant (`--controller`), when given.
    pub controller: Option<String>,
    /// The controller key input (`--controller-key`), when given: a PEM, or the whole block
    /// `ikigai-gonk onboard init` prints, as read from the file or stdin.
    pub key_input: Option<String>,
}

/// What `setup` did, line by line, and whether it wrote anything.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// One line per file and decision.
    pub lines: Vec<String>,
    /// Whether anything was written.
    pub wrote: bool,
}

impl Report {
    fn say(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }
}

/// The controller key as given: the PEM, and the controller the block names, if it does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ControllerKey {
    /// The one `PUBLIC KEY` block, ending in a newline.
    pub pem: String,
    /// The `controller = "…"` line of gonk's printed table, when it carries one uncommented.
    pub controller: Option<String>,
}

/// Read a controller key from `text`: a PEM file, or everything `ikigai-gonk onboard init`
/// printed (the `[onboard]` table and the PEM), so that output can be piped in as it is.
///
/// ```
/// use ikigai_layers::onboard::setup::read_controller_key;
///
/// let printed = "the layer host's [onboard] table, for its layers.toml:\n\n[onboard]\n\
///     grants = \"grants\"\ncontroller     = \"brian\"\n# trust_dir = \"…\"\n\n\
///     -----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEAGb9ECWmEzf6FQbrBZ9w7lshQhqowtrbLDFw4rXAxZuE=\n\
///     -----END PUBLIC KEY-----\n";
/// let key = read_controller_key(printed).unwrap();
/// assert_eq!(key.controller.as_deref(), Some("brian"));
/// assert!(key.pem.starts_with("-----BEGIN PUBLIC KEY-----\n"));
/// assert!(key.pem.ends_with("-----END PUBLIC KEY-----\n"));
/// assert!(read_controller_key("no key here").is_err());
/// ```
pub fn read_controller_key(text: &str) -> Result<ControllerKey, String> {
    let begins = text.matches(PEM_BEGIN).count();
    if begins != 1 {
        return Err(format!(
            "the controller key holds {begins} `{PEM_BEGIN}` blocks; give exactly one (the PEM \
             `ikigai-gonk onboard init` prints, or its whole output)"
        ));
    }
    let start = text.find(PEM_BEGIN).expect("counted above");
    let end = text[start..]
        .find(PEM_END)
        .map(|i| start + i + PEM_END.len())
        .ok_or_else(|| format!("the controller key's PEM has no `{PEM_END}` line"))?;
    let pem = format!("{}\n", text[start..end].trim());
    check_public_key_pem(&pem).map_err(|e| format!("the controller key: {e}"))?;
    let controller = text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("controller") && !l.starts_with('#'))
        .filter_map(|l| toml::from_str::<toml::Table>(l).ok())
        .find_map(|t| t.get("controller")?.as_str().map(str::to_string));
    Ok(ControllerKey { pem, controller })
}

/// Check that `dir` is a door's QUIC certificate directory, as cli's `serve quic://` keeps it
/// (`server.crt` and `server.key`), and return it absolute.
pub fn check_door_dir(dir: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if !absolute.is_dir() {
        return Err(format!(
            "--door-dir {} is not a directory: give the door's QUIC certificate directory (the \
             one cli's `serve quic://` keeps server.crt, server.key and clients/ in)",
            absolute.display()
        ));
    }
    let missing: Vec<&str> = ["server.crt", "server.key"]
        .into_iter()
        .filter(|f| !absolute.join(f).is_file())
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "--door-dir {} holds no {}, so it is not a door's QUIC certificate directory; the \
             trust directory is that directory's clients/, and with any other a node stays in \
             the lobby",
            absolute.display(),
            missing.join(" or ")
        ));
    }
    Ok(absolute)
}

/// Check the door's instance name by the rule the host applies to it
/// ([`lobby_principal`]): a name it refuses makes the door refuse every stranger.
pub fn check_door_name(name: &str) -> Result<(), String> {
    lobby_principal(name, &"0".repeat(64))
        .map(|_| ())
        .map_err(|e| format!("--door-name: {e}; the lobby refuses every stranger at such a door"))
}

/// The trust directory for a door directory: its `clients/`.
pub fn trust_dir_of(door_dir: &Path) -> PathBuf {
    door_dir.join("clients")
}

/// The embedded manifest, with `controller` in place of the participant it names.
///
/// ```
/// use ikigai_layers::onboard::setup::manifest_text;
///
/// let text = manifest_text("chris");
/// assert!(text.contains("\nchris = [\"read\", \"feed\", \"say\"]"));
/// assert!(!text.contains("\nbrian ="));
/// assert!(manifest_text("urn:collab:participant:dana").contains("\n\"urn:collab:participant:dana\" = "));
/// ```
pub fn manifest_text(controller: &str) -> String {
    let bare = !controller.is_empty()
        && controller
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let key = if bare {
        controller.to_string()
    } else {
        toml::Value::String(controller.to_string()).to_string()
    };
    MANIFEST.replace(
        &format!("\n{EMBEDDED_CONTROLLER} = "),
        &format!("\n{key} = "),
    )
}

/// The `[onboard]` table setup appends.
fn table_text(controller: &str, trust_dir: &Path) -> String {
    let s = |v: &str| toml::Value::String(v.to_string()).to_string();
    format!(
        "\n# Onboarding (ledger #2009), written by `ikigai-layers onboard setup`. trust_dir is the\n\
         # door's own client directory (--door-dir's clients/); with any other, an approved node\n\
         # stays in the lobby. \"live\": the door reads it at every handshake.\n\
         [onboard]\n\
         grants = {}\n\
         controller = {}\n\
         controller_key = {}\n\
         trust_dir = {}\n\
         trust_reload = \"live\"\n",
        s(GRANTS_LAYER),
        s(controller),
        s(KEY_FILE),
        s(&trust_dir.to_string_lossy()),
    )
}

/// The first place `kept` differs from `wanted`, for a human.
fn difference(kept: &str, wanted: &str) -> String {
    let (k, w): (Vec<&str>, Vec<&str>) = (kept.lines().collect(), wanted.lines().collect());
    match (0..k.len().max(w.len())).find(|&i| k.get(i) != w.get(i)) {
        Some(i) => format!(
            "first difference at line {}: kept {:?}, embedded {:?} ({} lines kept, {} embedded)",
            i + 1,
            k.get(i).copied().unwrap_or("<end of file>"),
            w.get(i).copied().unwrap_or("<end of file>"),
            k.len(),
            w.len()
        ),
        None => "they differ only in line endings or a final newline".to_string(),
    }
}

/// Whether two paths name one directory, resolving what exists of each.
fn same_dir(a: &Path, b: &Path) -> bool {
    fn resolve(p: &Path) -> PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(name)) => resolve(parent).join(name),
            _ => p.to_path_buf(),
        }
    }
    resolve(a) == resolve(b)
}

/// The `[onboard]` table of `layers.toml` as written, when it has one.
fn existing_table(config_dir: &Path) -> Result<(Option<String>, Option<toml::Table>), String> {
    let path = config_dir.join(HOST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((None, None)),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let parsed: toml::Table =
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let table = match parsed.get("onboard") {
        None => None,
        Some(toml::Value::Table(t)) => Some(t.clone()),
        Some(_) => return Err(format!("{}: `onboard` is not a table", path.display())),
    };
    Ok((Some(text), table))
}

/// How an existing `[onboard]` table differs from what setup would write.
fn table_differences(
    table: &toml::Table,
    config_dir: &Path,
    controller: &str,
    trust_dir: &Path,
) -> Vec<String> {
    let mut out = Vec::new();
    let get = |k: &str| table.get(k).and_then(toml::Value::as_str);
    match get("grants") {
        Some(GRANTS_LAYER) => {}
        Some(other) => out.push(format!(
            "grants is {other:?}; setup writes {GRANTS_LAYER:?}"
        )),
        None => out.push(format!("grants is missing; setup writes {GRANTS_LAYER:?}")),
    }
    let iri = |k: &str| {
        Participant::from_manifest_key(k)
            .map(|p| p.iri().to_string())
            .ok()
    };
    match get("controller") {
        Some(c) if iri(c).is_some() && iri(c) == iri(controller) => {}
        Some(c) => out.push(format!(
            "controller is {c:?}; setup was given {controller:?}"
        )),
        None => out.push(format!(
            "controller is missing; setup writes {controller:?}"
        )),
    }
    if get("controller_key").is_none() {
        out.push(format!(
            "controller_key is missing; setup writes {KEY_FILE:?}"
        ));
    }
    let wanted = trust_dir.to_string_lossy();
    match get("trust_dir") {
        Some(t) if same_dir(&config_dir.join(t), trust_dir) => {}
        Some(t) => out.push(format!(
            "trust_dir is {t:?}, which is not the door's client directory; set trust_dir = {wanted:?}"
        )),
        None => out.push(format!(
            "trust_dir is missing; set trust_dir = {wanted:?}"
        )),
    }
    out
}

/// A scratch copy of a configuration, removed when dropped.
struct Stage(PathBuf);

impl Stage {
    fn of(config_dir: &Path, key_file: &str) -> Result<Stage, String> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-layers-setup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let stage = Stage(dir);
        let copy = |rel: &Path| -> Result<(), String> {
            let from = config_dir.join(rel);
            let to = stage.0.join(rel);
            std::fs::create_dir_all(to.parent().expect("a file under the stage"))
                .and_then(|()| std::fs::copy(&from, &to).map(|_| ()))
                .map_err(|e| format!("stage {}: {e}", from.display()))
        };
        std::fs::create_dir_all(&stage.0).map_err(|e| format!("stage: {e}"))?;
        for sub in ["layers", "collabs"] {
            let entries = match std::fs::read_dir(config_dir.join(sub)) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", config_dir.join(sub).display())),
            };
            for entry in entries {
                let entry = entry.map_err(|e| format!("read {sub}/: {e}"))?;
                if entry.file_type().is_ok_and(|t| t.is_file()) {
                    copy(&Path::new(sub).join(entry.file_name()))?;
                }
            }
        }
        for file in [
            "participants.ttl",
            "enrollment.toml",
            crate::config::ENTITLEMENTS_FILE,
            HOST_FILE,
            key_file,
        ] {
            if config_dir.join(file).is_file() {
                copy(Path::new(file))?;
            }
        }
        Ok(stage)
    }

    fn write(&self, rel: &str, text: &str) -> Result<(), String> {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().expect("a file under the stage"))
            .and_then(|()| std::fs::write(&path, text))
            .map_err(|e| format!("stage {rel}: {e}"))
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Write the layer host's half of onboarding into `opts.config_dir`. See the module docs.
///
/// `Err` is a refusal, and nothing was written. `Ok` says what was written, kept and left;
/// the caller runs [`check`] after it for whether a node could still be stuck.
pub fn setup(opts: &Options) -> Result<Report, String> {
    let dir = &opts.config_dir;
    if !dir.is_dir() {
        return Err(format!(
            "{} is not a directory: give the layer host's config directory",
            dir.display()
        ));
    }
    let door_dir = check_door_dir(&opts.door_dir)?;
    check_door_name(&opts.door_name)?;
    let trust_dir = trust_dir_of(&door_dir);
    let key = opts
        .key_input
        .as_deref()
        .map(read_controller_key)
        .transpose()?;
    let (host_text, table) = existing_table(dir)?;

    // Who the controller is: --controller, else the printed block's, else the table's.
    let from_key = key.as_ref().and_then(|k| k.controller.clone());
    let from_table = table
        .as_ref()
        .and_then(|t| t.get("controller")?.as_str().map(str::to_string));
    let controller = match (&opts.controller, &from_key) {
        (Some(given), Some(printed))
            if Participant::from_manifest_key(given).map(|p| p.iri().to_string())
                != Participant::from_manifest_key(printed).map(|p| p.iri().to_string()) =>
        {
            return Err(format!(
                "--controller is {given:?}, and the controller key's block names {printed:?}; \
                 give one"
            ))
        }
        (Some(given), _) => given.clone(),
        (None, Some(printed)) => printed.clone(),
        (None, None) => from_table.clone().ok_or_else(|| {
            "no controller: pass --controller <participant> (the participant who approves \
             nodes, as a manifest names it, e.g. `brian`)"
                .to_string()
        })?,
    };
    Participant::from_manifest_key(&controller).map_err(|e| format!("--controller: {e}"))?;

    let mut report = Report::default();
    // Every write, planned: (path under the config dir, text).
    let mut writes: Vec<(String, String)> = Vec::new();
    let mut plan = |rel: &str, wanted: String, report: &mut Report| -> Result<bool, String> {
        let path = dir.join(rel);
        match std::fs::read_to_string(&path) {
            Ok(have) if have == wanted => {
                report.say(format!("unchanged {rel}"));
                Ok(false)
            }
            Ok(have) => {
                report.say(format!(
                    "kept      {rel}: it differs from the embedded copy, {}",
                    difference(&have, &wanted)
                ));
                Ok(false)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                writes.push((rel.to_string(), wanted));
                Ok(true)
            }
            Err(e) => Err(format!("read {}: {e}", path.display())),
        }
    };
    let mut layers_written = false;
    for (rel, text) in LAYER_FILES {
        layers_written |= plan(rel, text.to_string(), &mut report)?;
    }
    plan(MANIFEST_FILE, manifest_text(&controller), &mut report)?;

    // The key file: the table's name for it, or KEY_FILE for a table setup writes.
    let key_file = table
        .as_ref()
        .and_then(|t| t.get("controller_key")?.as_str().map(str::to_string))
        .unwrap_or_else(|| KEY_FILE.to_string());
    let mut onboard_written = false;
    match (&key, &table) {
        (Some(key), _) => {
            let path = dir.join(&key_file);
            match std::fs::read_to_string(&path) {
                Ok(have) if have.trim() == key.pem.trim() => {
                    report.say(format!("unchanged {key_file}"));
                }
                Ok(_) => report.say(format!(
                    "kept      {key_file}: it holds a different public key than the one given; \
                     nothing replaces a controller key but the operator"
                )),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    writes.push((key_file.clone(), key.pem.clone()));
                    onboard_written = true;
                }
                Err(e) => return Err(format!("read {}: {e}", path.display())),
            }
        }
        (None, None) => {
            return Err(
                "no controller key: pass --controller-key <file>, or --controller-key - to read \
                 it from stdin (the PEM `ikigai-gonk onboard init` prints, or its whole output)"
                    .to_string(),
            )
        }
        (None, Some(_)) => {}
    }

    let mut new_host_text = None;
    match &table {
        Some(table) => {
            let differences = table_differences(table, dir, &controller, &trust_dir);
            if differences.is_empty() {
                report.say(format!("unchanged {HOST_FILE} [onboard]"));
            } else {
                for d in differences {
                    report.say(format!(
                        "differs   {HOST_FILE} [onboard], kept as it is: {d}"
                    ));
                }
            }
        }
        None => {
            let mut text = host_text.clone().unwrap_or_default();
            if !text.is_empty() && !text.ends_with('\n') {
                text.push('\n');
            }
            text.push_str(&table_text(&controller, &trust_dir));
            new_host_text = Some(text);
            onboard_written = true;
        }
    }

    // Stage every write over a copy and load it as the host would, before touching anything.
    let stage = Stage::of(dir, &key_file)?;
    for (rel, text) in &writes {
        stage.write(rel, text)?;
    }
    if let Some(text) = &new_host_text {
        stage.write(HOST_FILE, text)?;
    }
    let (config, _) = derive(&stage.0).map_err(|e| {
        format!("nothing written: the configuration would not load with these files: {e}")
    })?;
    if new_host_text.is_some() {
        let onboard = load_onboard(&stage.0, &config.layers)
            .map_err(|e| format!("nothing written: the [onboard] table would not load: {e}"))?
            .expect("written above");
        check_controller_says(&onboard, &config.collabs)
            .map_err(|e| format!("nothing written: {e}"))?;
    }
    drop(stage);

    // Commit.
    for (rel, text) in &writes {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        write_atomic(&path, text)?;
        report.say(format!("wrote     {rel}"));
        report.wrote = true;
    }
    if let Some(text) = &new_host_text {
        let path = dir.join(HOST_FILE);
        if host_text.is_some() {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let backup = dir.join(format!("{HOST_FILE}.{stamp}.bak"));
            std::fs::copy(&path, &backup)
                .map_err(|e| format!("back up {}: {e}", path.display()))?;
            report.say(format!("backup    {}", backup.display()));
        }
        write_atomic(&path, text)?;
        report.say(format!(
            "wrote     {HOST_FILE} [onboard] (trust_dir = {})",
            trust_dir.display()
        ));
        report.wrote = true;
    }

    let (_, enrollment) = derive(dir).map_err(|e| e.to_string())?;
    if enrollment.is_written(dir) {
        report.say("unchanged grants.json and clients.json");
    } else {
        enrollment.write(dir)?;
        report.say("wrote     grants.json and clients.json (what `ikigai-layers grants` writes)");
        report.wrote = true;
    }

    if layers_written || onboard_written {
        report.say(
            "restart the door: the layer host reads layer definitions and [onboard] only when it \
             opens",
        );
    } else {
        report.say("no restart: manifests, grants.json and clients.json are read live");
    }
    if !report.wrote {
        report.say("nothing written: everything is in place");
    }
    Ok(report)
}

/// Every way the configuration under `config_dir` would leave a node waiting in the lobby,
/// for a door whose QUIC certificate directory is `door_dir` and instance name `door_name`.
/// Empty is none. Read-only.
pub fn check(config_dir: &Path, door_dir: &Path, door_name: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let door_dir = check_door_dir(door_dir).map_err(|e| problems.push(e)).ok();
    if let Err(e) = check_door_name(door_name) {
        problems.push(e);
    }
    let (config, enrollment): (Config, _) = match derive(config_dir) {
        Ok(derived) => derived,
        Err(e) => {
            problems.push(format!("the configuration does not load: {e}"));
            return problems;
        }
    };
    if !config.layers.values().any(|def| def.lobby) {
        problems.push(
            "no lobby layer (a channel with `lobby = true`): the door admits no stranger at all"
                .to_string(),
        );
    }
    match load_onboard(config_dir, &config.layers) {
        Ok(None) => problems.push(format!(
            "{HOST_FILE} has no [onboard] table: no entitlement is ever applied"
        )),
        Err(e) => problems.push(format!("the [onboard] table does not load: {e}")),
        Ok(Some(onboard)) => {
            if let Some(door_dir) = &door_dir {
                let wanted = trust_dir_of(door_dir);
                if !same_dir(&onboard.trust_dir, &wanted) {
                    problems.push(format!(
                        "trust_dir is {}, not the door's client directory {}: an approved \
                         certificate is written where the door never reads it",
                        onboard.trust_dir.display(),
                        wanted.display()
                    ));
                }
            }
            if let Err(e) = check_controller_says(&onboard, &config.collabs) {
                problems.push(e.to_string());
            }
        }
    }
    if !enrollment.is_written(config_dir) {
        problems.push(
            "grants.json and clients.json are not what the manifests derive: run `ikigai-layers \
             grants` (or setup)"
                .to_string(),
        );
    }
    problems
}
