//! The host's configuration: layer definitions, the participants graph and the
//! collaboration manifests, read from one directory (the layer host's config home).
//!
//! ```text
//! <config dir>/
//!   layers/{layer}.toml          a layer's definition (model, shapes, context, options)
//!   layers/{file}.ttl, .jsonld   the shapes and context a definition names
//!   participants.ttl             who each principal is (served at urn:layers:participants)
//!   collabs/{name}.toml          a collaboration's manifest (served at urn:collab:{name})
//!   enrollment.toml              certificate fingerprints, read only by the grants generator
//!   entitlements.toml            WRITTEN by the host: what verified entitlements grant
//!                                (crate::onboard), read by the grants generator beside the
//!                                manifests
//!   layers.toml                  optional: the host's own options ([`load_match_budget`])
//! ```
//!
//! Everything is checked when it is read, and a broken file is the OPERATOR's error: it is
//! reported as the host's ([`ConfigError`]), never as a caller's bad argument.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use ikigai_store::budget::TimeBudget;
use oxrdf::{Term, Triple};
use serde::Deserialize;

use crate::name::{is_layer_name, is_plain_file_name, is_short_name, Participant};
use crate::rdf::{self, object_iri, subject_iri};

/// A configuration the host cannot serve, with the file it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "layer host configuration: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

fn err<T>(message: impl Into<String>) -> Result<T, ConfigError> {
    Err(ConfigError(message.into()))
}

/// The default bound on one entry's content: 16 KiB.
pub const DEFAULT_ENTRY_MAX: usize = 16 * 1024;
/// The largest bound a definition may set: 1 MiB.
pub const MAX_ENTRY_MAX: usize = 1024 * 1024;

/// An access model: what a layer's write verbs do and what state it holds. A model is
/// code and a layer is configuration, so this set is closed and small.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Model {
    /// Append-only entries, one graph per author: `say`, and reads.
    Channel,
    /// A tuplespace fronting an ikigai-intray space: `out` drops a tuple, `take` removes
    /// one atomically and leaves a receipt naming its taker, and reads.
    Tuplespace,
}

impl Model {
    /// The word a definition and `ik:layerModel` use.
    pub fn as_str(self) -> &'static str {
        match self {
            Model::Channel => "channel",
            Model::Tuplespace => "tuplespace",
        }
    }

    /// The verbs a manifest may grant on a layer of this model, each the `{verb}` of
    /// `urn:cap:layer:{verb}:{layer}`.
    pub fn verbs(self) -> &'static [&'static str] {
        match self {
            Model::Channel => &["read", "feed", "say"],
            Model::Tuplespace => &["read", "feed", "out", "take"],
        }
    }

    fn parse(word: &str) -> Result<Model, String> {
        match word {
            "channel" => Ok(Model::Channel),
            "tuplespace" => Ok(Model::Tuplespace),
            // Named by the design and not built in this version: refused loudly rather than
            // served as something it is not.
            "graph" => {
                Err("model `graph` is not built in this version (ledger #1170, arc 14)".into())
            }
            other => Err(format!(
                "unknown model `{other}` (this version builds: channel, tuplespace)"
            )),
        }
    }
}

/// One layer, as its definition file says.
#[derive(Clone, Debug)]
pub struct LayerDef {
    /// The layer's name: the file's stem.
    pub name: String,
    /// Its access model.
    pub model: Model,
    /// A human label, if the definition gives one.
    pub label: Option<String>,
    /// The closed SHACL shapes every write must conform to, as Turtle.
    pub shapes: String,
    /// The classes the shapes target (`sh:targetClass`): an entry must be typed with one,
    /// and only with these, so a shape always applies to it.
    pub target_classes: BTreeSet<String>,
    /// The egress context: this layer's own allowlist of terms, as JSON-LD.
    pub context: String,
    /// The bound on one entry's (or tuple's) content, in bytes.
    pub entry_max: usize,
    /// The predicate `latest about=` reads (default `dcterms:subject`). A channel's only.
    pub about: String,
    /// A tuplespace's only: whether a tuple carrying `ik:addressedTo` may be taken only by a
    /// participant it names (default `false`).
    pub addressed: bool,
    /// A tuplespace's only: whether `rd` takes a `match=` template (default `false`).
    ///
    /// Off unless the definition says `match = true`, because a template is the CALLER's
    /// SPARQL ASK, evaluated against every pending tuple: the store's limits bound its size
    /// and nesting, and a cross join of a few triple patterns is small and shallow and still
    /// runs for minutes (ledger #1327). The host's time budget ([`load_match_budget`])
    /// answers its caller in time, but what the evaluator cannot stop goes on holding a core
    /// until it ends. So a layer whose readers include anyone it would not hand a thread to
    /// keeps it off; `rd` without a template, `out` and `take` are the same either way.
    pub matching: bool,
    /// The collaborations this layer is declared shared with. `None` means private: at
    /// most one manifest may mount it.
    pub shared: Option<BTreeSet<String>>,
    /// A channel's only: whether this layer is an enrollment LOBBY (`lobby = true`, ledger
    /// #2009). A door may admit a certificate nobody enrolled to `say` on a lobby and nothing
    /// else ([`crate::onboard::lobby_capability`]); the host accepts a lobby entry from a
    /// principal the participants graph does not name, so long as its name carries its
    /// certificate's fingerprint, and holds every lobby to the bounds in [`crate::onboard`]:
    /// at most [`crate::onboard::LOBBY_ENTRY_MAX`] bytes an entry, and a rate.
    pub lobby: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayerToml {
    model: String,
    shapes: String,
    context: String,
    label: Option<String>,
    entry_max: Option<toml::Value>,
    about: Option<String>,
    addressed: Option<bool>,
    #[serde(rename = "match")]
    matching: Option<bool>,
    shared: Option<Vec<String>>,
    lobby: Option<bool>,
}

/// Read every `layers/*.toml` under `dir`. No `layers/` directory is a host with no
/// layers, not an error.
pub fn load_layers(dir: &Path) -> Result<BTreeMap<String, LayerDef>, ConfigError> {
    let layers_dir = dir.join("layers");
    let mut layers = BTreeMap::new();
    for path in toml_files(&layers_dir)? {
        let def = load_layer(&layers_dir, &path)?;
        layers.insert(def.name.clone(), def);
    }
    Ok(layers)
}

fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, ConfigError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return err(format!("read {}: {e}", dir.display())),
    };
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| ConfigError(format!("read {}: {e}", dir.display())))?
            .path();
        if path.extension().is_some_and(|x| x == "toml") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn read(path: &Path) -> Result<String, ConfigError> {
    std::fs::read_to_string(path).map_err(|e| ConfigError(format!("read {}: {e}", path.display())))
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn load_layer(layers_dir: &Path, path: &Path) -> Result<LayerDef, ConfigError> {
    let name = stem(path);
    let at = path.display();
    if !is_layer_name(&name) {
        return err(format!(
            "{at}: `{name}` is not a layer name (lowercase [a-z0-9-], at most {} characters, \
             so its longest MCP tool name fits in 64, and no Windows device name, since it \
             names a directory)",
            crate::name::MAX_LAYER_NAME
        ));
    }
    let parsed: LayerToml =
        toml::from_str(&read(path)?).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let model = Model::parse(&parsed.model).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    // An option of another model is a definition saying something it does not mean, so it
    // is refused like a typo'd key rather than ignored.
    match model {
        Model::Channel if parsed.addressed.is_some() => {
            return err(format!(
                "{at}: `addressed` is a tuplespace option; a channel has no take"
            ))
        }
        Model::Channel if parsed.matching.is_some() => {
            return err(format!(
                "{at}: `match` is a tuplespace option (its `rd match=` template); a channel \
                 has no rd"
            ))
        }
        Model::Tuplespace if parsed.lobby.is_some() => {
            return err(format!(
                "{at}: `lobby` is a channel option: an enrollment request is an entry someone \
                 says"
            ))
        }
        Model::Tuplespace if parsed.about.is_some() => {
            return err(format!(
                "{at}: `about` is a channel option (its `latest about=` read); a tuplespace \
                 has no latest"
            ))
        }
        _ => {}
    }
    let shapes = read(&beside(layers_dir, &parsed.shapes, path)?)?;
    let target_classes = check_shapes(&shapes)
        .map_err(|e| ConfigError(format!("{at}: shapes `{}`: {e}", parsed.shapes)))?;
    let shapes = skolemize(
        &shapes,
        &format!("{}:shapes:", crate::name::layer_iri(&name)),
    )
    .map_err(|e| ConfigError(format!("{at}: shapes `{}`: {e}", parsed.shapes)))?;
    let context = read(&beside(layers_dir, &parsed.context, path)?)?;
    check_context(&context)
        .map_err(|e| ConfigError(format!("{at}: context `{}`: {e}", parsed.context)))?;
    let entry_max = match &parsed.entry_max {
        None => DEFAULT_ENTRY_MAX,
        Some(value) => {
            parse_size(value).map_err(|e| ConfigError(format!("{at}: entry_max {e}")))?
        }
    };
    let about = match parsed.about {
        None => rdf::DCT_SUBJECT.to_string(),
        Some(iri) => {
            oxrdf::NamedNode::new(&iri)
                .map_err(|e| ConfigError(format!("{at}: about `{iri}` is not an IRI: {e}")))?;
            iri
        }
    };
    let lobby = parsed.lobby.unwrap_or(false);
    if lobby && entry_max > crate::onboard::LOBBY_ENTRY_MAX {
        return err(format!(
            "{at}: a lobby admits certificates nobody enrolled, so its entry_max is at most \
             {} bytes, and this says {entry_max}",
            crate::onboard::LOBBY_ENTRY_MAX
        ));
    }
    let shared = match parsed.shared {
        None => None,
        Some(names) => {
            if let Some(bad) = names.iter().find(|n| !is_short_name(n)) {
                return err(format!(
                    "{at}: shared names `{bad}`, which is no collaboration name"
                ));
            }
            Some(names.into_iter().collect())
        }
    };
    Ok(LayerDef {
        name,
        model,
        label: parsed.label,
        shapes,
        target_classes,
        context,
        entry_max,
        about,
        addressed: parsed.addressed.unwrap_or(false),
        matching: parsed.matching.unwrap_or(false),
        shared,
        lobby,
    })
}

/// A file a definition names, which must sit in `layers/` itself: a bare file name
/// ([`is_plain_file_name`]: no separator, no drive prefix, no device name), so a definition
/// cannot read anything outside the host's own configuration on any platform.
fn beside(layers_dir: &Path, file: &str, def: &Path) -> Result<PathBuf, ConfigError> {
    if !is_plain_file_name(file) {
        return err(format!(
            "{}: `{file}` must be a file name in layers/, not a path",
            def.display()
        ));
    }
    Ok(layers_dir.join(file))
}

/// `16384`, `"16KiB"`, `"1MiB"` or `"512B"`, at most [`MAX_ENTRY_MAX`].
fn parse_size(value: &toml::Value) -> Result<usize, String> {
    let bytes = match value {
        toml::Value::Integer(n) if *n > 0 => *n as u64,
        toml::Value::String(s) => {
            let s = s.trim();
            let (digits, unit) = s
                .find(|c: char| !c.is_ascii_digit())
                .map(|i| s.split_at(i))
                .unwrap_or((s, ""));
            let n: u64 = digits
                .parse()
                .map_err(|_| format!("`{s}` is not a size (e.g. 16KiB)"))?;
            let factor = match unit.trim() {
                "" | "B" => 1,
                "KiB" => 1024,
                "MiB" => 1024 * 1024,
                other => return Err(format!("unit `{other}` (use B, KiB or MiB)")),
            };
            n.saturating_mul(factor)
        }
        _ => return Err("must be a positive integer or a size string".into()),
    };
    if bytes == 0 || bytes > MAX_ENTRY_MAX as u64 {
        return Err(format!("{bytes} is outside 1..={MAX_ENTRY_MAX} bytes"));
    }
    Ok(bytes as usize)
}

/// The shapes must parse, every node shape must be CLOSED (`sh:closed true`), and at least
/// one must target a class. Returns the targeted classes.
///
/// Why closed, and why a class: admission is "the entry conforms to the layer's shapes",
/// and SHACL answers `conforms` for anything no shape targets or constrains. An open shape
/// admits every predicate it does not mention, and an entry of a class no shape targets is
/// checked by nothing. So the host refuses both shapes at load and entries at `say`.
pub fn check_shapes(turtle: &str) -> Result<BTreeSet<String>, String> {
    let triples = rdf::parse_turtle(turtle)?;
    let mut node_shapes = BTreeSet::new();
    let mut closed = BTreeSet::new();
    let mut targets = BTreeSet::new();
    for t in &triples {
        // A blank-node shape is still a shape: name every subject by its Turtle form.
        let s = t.subject.to_string();
        match t.predicate.as_str() {
            rdf::RDF_TYPE if object_iri(&t.object) == Some(rdf::SH_NODE_SHAPE) => {
                node_shapes.insert(s);
            }
            rdf::SH_TARGET_CLASS => {
                node_shapes.insert(s.clone());
                match object_iri(&t.object) {
                    Some(class) => {
                        targets.insert(class.to_string());
                    }
                    None => return Err(format!("{s}: sh:targetClass must be an IRI")),
                }
            }
            rdf::SH_CLOSED => {
                if let Term::Literal(l) = &t.object {
                    if l.value() == "true" {
                        closed.insert(s);
                    }
                }
            }
            _ => {}
        }
    }
    if let Some(open) = node_shapes.iter().find(|s| !closed.contains(*s)) {
        return Err(format!(
            "node shape {open} is not closed (`sh:closed true`): an open shape admits every \
             predicate it does not mention"
        ));
    }
    if targets.is_empty() {
        return Err(
            "no node shape targets a class (`sh:targetClass`), so nothing could be \
                    admitted"
                .into(),
        );
    }
    Ok(targets)
}

/// The shapes with every blank node named `{prefix}n{i}`, in order of first appearance.
///
/// SHACL is written with blank nodes (property shapes, `sh:in` and `sh:ignoredProperties`
/// lists), and a graph this host serves carries none. The SAME skolemized text is what
/// `urn:layer:{layer}:shapes` serves and what admission validates against, so the shapes a
/// participant reads are exactly the shapes that judged its entry. Naming a node changes
/// nothing SHACL means: a property shape or a list cell may be an IRI.
pub fn skolemize(turtle: &str, prefix: &str) -> Result<String, String> {
    use oxrdf::{NamedNode, NamedOrBlankNode};
    let mut names: BTreeMap<String, NamedNode> = BTreeMap::new();
    let mut name = |b: &oxrdf::BlankNode| -> NamedNode {
        let next = names.len();
        names
            .entry(b.as_str().to_string())
            .or_insert_with(|| rdf::node(&format!("{prefix}n{next}")))
            .clone()
    };
    let triples: Vec<Triple> = rdf::parse_turtle(turtle)?
        .into_iter()
        .map(|t| {
            let subject = match t.subject {
                NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::NamedNode(name(&b)),
                other => other,
            };
            let object = match t.object {
                Term::BlankNode(b) => Term::NamedNode(name(&b)),
                other => other,
            };
            Triple::new(subject, t.predicate, object)
        })
        .collect();
    Ok(rdf::to_turtle_with(
        &triples,
        &[("sh", "http://www.w3.org/ns/shacl#")],
    ))
}

/// A layer's context must be an inline JSON-LD context with no `@vocab`, `@base` or
/// `@import` anywhere and no remote context: it is an ALLOWLIST, and each of those would
/// let terms through that the document does not define.
pub fn check_context(json: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("not JSON: {e}"))?;
    let context = value.get("@context").ok_or("no `@context` member")?;
    check_context_value(context)
}

fn check_context_value(value: &serde_json::Value) -> Result<(), String> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, inner) in map {
                if matches!(key.as_str(), "@vocab" | "@base" | "@import") {
                    return Err(format!(
                        "`{key}` is refused: a layer context is an allowlist, and `{key}` would \
                         admit terms it does not define"
                    ));
                }
                if key == "@context" {
                    check_context_value(inner)?;
                } else if let serde_json::Value::Object(term) = inner {
                    if let Some(scoped) = term.get("@context") {
                        check_context_value(scoped)?;
                    }
                }
            }
            Ok(())
        }
        serde_json::Value::Array(items) => items.iter().try_for_each(check_context_value),
        serde_json::Value::String(remote) => Err(format!(
            "remote context `{remote}` is refused: a layer context is inline, so what crosses \
             is what this file says"
        )),
        serde_json::Value::Null => Ok(()),
        _ => Err("a context is an object, an array of objects, or null".into()),
    }
}

/// One participant in the participants graph.
#[derive(Clone, Debug)]
pub struct Member {
    /// Its name.
    pub participant: Participant,
    /// Its `rdf:type`s (at least one).
    pub types: BTreeSet<String>,
    /// Its `rdfs:label`, if any.
    pub label: Option<String>,
}

/// The participants graph: who each principal is.
#[derive(Clone, Debug, Default)]
pub struct Participants {
    /// Every triple, as read (no blank nodes).
    pub triples: Vec<Triple>,
    /// The participants, by IRI.
    pub members: BTreeMap<String, Member>,
}

impl Participants {
    /// Whether `iri` is a participant.
    pub fn names(&self, iri: &str) -> bool {
        self.members.contains_key(iri)
    }
}

/// Read `participants.ttl` under `dir`. Absent is an empty graph (nobody may write).
///
/// Every subject must be a participant IRI ([`Participant::parse`]) with at least one
/// `rdf:type`, and no blank node may appear: the graph is served as it is read.
pub fn load_participants(dir: &Path) -> Result<Participants, ConfigError> {
    let path = dir.join("participants.ttl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Participants::default()),
        Err(e) => return err(format!("read {}: {e}", path.display())),
    };
    let at = path.display();
    let triples = rdf::parse_turtle(&text).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let mut members: BTreeMap<String, Member> = BTreeMap::new();
    for t in &triples {
        if rdf::has_blank_node(t) {
            return err(format!("{at}: a blank node; name every node by an IRI"));
        }
        let Some(subject) = subject_iri(&t.subject) else {
            continue;
        };
        if !members.contains_key(subject) {
            let participant =
                Participant::parse(subject).map_err(|e| ConfigError(format!("{at}: {e}")))?;
            members.insert(
                subject.to_string(),
                Member {
                    participant,
                    types: BTreeSet::new(),
                    label: None,
                },
            );
        }
        let member = members.get_mut(subject).expect("inserted above");
        match t.predicate.as_str() {
            rdf::RDF_TYPE => {
                if let Some(class) = object_iri(&t.object) {
                    member.types.insert(class.to_string());
                }
            }
            rdf::RDFS_LABEL => {
                if let Term::Literal(l) = &t.object {
                    member.label = Some(l.value().to_string());
                }
            }
            _ => {}
        }
    }
    if let Some((iri, _)) = members.iter().find(|(_, m)| m.types.is_empty()) {
        return err(format!("{at}: participant {iri} has no rdf:type"));
    }
    Ok(Participants { triples, members })
}

/// One layer a manifest mounts, and who holds which verbs on it.
#[derive(Clone, Debug)]
pub struct Mount {
    /// The layer.
    pub layer: String,
    /// Each participant's verbs.
    pub grants: BTreeMap<Participant, BTreeSet<String>>,
}

/// A collaboration's manifest.
#[derive(Clone, Debug)]
pub struct Manifest {
    /// Its name: the file's stem.
    pub name: String,
    /// A human label, if the manifest gives one.
    pub label: Option<String>,
    /// The layers it mounts, in file order.
    pub mounts: Vec<Mount>,
}

/// Read every `collabs/*.toml` under `dir`, checked against the layers and participants,
/// and the ISOLATION rule (Brian, 2026-10-10: separate channels, separate workflows, no
/// overlap unless decided):
///
/// - a layer that declares no `shared` list may be mounted by at most ONE manifest;
/// - a layer that declares `shared = [..]` may be mounted only by manifests it lists.
///
/// So two collaborations share a layer only when that layer's own definition records the
/// decision, and the grants generator, which reads only what this function accepts, can
/// never grant one collaboration's participant anything on another's layer.
pub fn load_collabs(
    dir: &Path,
    layers: &BTreeMap<String, LayerDef>,
    participants: &Participants,
) -> Result<BTreeMap<String, Manifest>, ConfigError> {
    let collabs_dir = dir.join("collabs");
    let mut manifests = BTreeMap::new();
    for path in toml_files(&collabs_dir)? {
        let manifest = load_manifest(&path, layers, participants)?;
        manifests.insert(manifest.name.clone(), manifest);
    }
    check_isolation(&manifests, layers)?;
    Ok(manifests)
}

fn load_manifest(
    path: &Path,
    layers: &BTreeMap<String, LayerDef>,
    participants: &Participants,
) -> Result<Manifest, ConfigError> {
    let name = stem(path);
    let at = path.display();
    if !is_short_name(&name) {
        return err(format!(
            "{at}: `{name}` is not a collaboration name (lowercase [a-z0-9-])"
        ));
    }
    let table: toml::Table =
        toml::from_str(&read(path)?).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let mut label = None;
    let mut mounts: Vec<Mount> = Vec::new();
    for (key, value) in &table {
        match (key.as_str(), value) {
            ("label", toml::Value::String(s)) => label = Some(s.clone()),
            ("mount", toml::Value::Array(items)) => {
                for item in items {
                    let toml::Value::Table(mount) = item else {
                        return err(format!("{at}: each [[mount]] is a table"));
                    };
                    let mount = load_mount(&at.to_string(), mount, layers, participants)?;
                    if mounts.iter().any(|m| m.layer == mount.layer) {
                        return err(format!("{at}: layer `{}` is mounted twice", mount.layer));
                    }
                    mounts.push(mount);
                }
            }
            (other, _) => {
                return err(format!(
                    "{at}: unknown key `{other}` (a manifest has `label` and [[mount]] tables)"
                ))
            }
        }
    }
    Ok(Manifest {
        name,
        label,
        mounts,
    })
}

fn load_mount(
    at: &str,
    mount: &toml::Table,
    layers: &BTreeMap<String, LayerDef>,
    participants: &Participants,
) -> Result<Mount, ConfigError> {
    let layer = match mount.get("layer") {
        Some(toml::Value::String(layer)) => layer.clone(),
        _ => return err(format!("{at}: a [[mount]] names its `layer`")),
    };
    let Some(def) = layers.get(&layer) else {
        return err(format!(
            "{at}: mounts layer `{layer}`, which no definition defines"
        ));
    };
    let mut grants = BTreeMap::new();
    for (key, value) in mount.iter().filter(|(k, _)| *k != "layer") {
        let participant =
            Participant::from_manifest_key(key).map_err(|e| ConfigError(format!("{at}: {e}")))?;
        if !participants.names(participant.iri()) {
            return err(format!(
                "{at}: layer `{layer}` grants {}, whom participants.ttl does not name",
                participant.iri()
            ));
        }
        let toml::Value::Array(items) = value else {
            return err(format!("{at}: `{key}` is a list of verbs"));
        };
        let mut verbs = BTreeSet::new();
        for item in items {
            let Some(verb) = item.as_str() else {
                return err(format!("{at}: `{key}` lists a verb that is not a string"));
            };
            if !def.model.verbs().contains(&verb) {
                return err(format!(
                    "{at}: `{key}` holds `{verb}` on `{layer}`, a {} layer, whose verbs are {}",
                    def.model.as_str(),
                    def.model.verbs().join(", ")
                ));
            }
            verbs.insert(verb.to_string());
        }
        if grants.insert(participant.clone(), verbs).is_some() {
            return err(format!(
                "{at}: {} is named twice on `{layer}`",
                participant.iri()
            ));
        }
    }
    Ok(Mount { layer, grants })
}

fn check_isolation(
    manifests: &BTreeMap<String, Manifest>,
    layers: &BTreeMap<String, LayerDef>,
) -> Result<(), ConfigError> {
    let mut mounted_by: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for manifest in manifests.values() {
        for mount in &manifest.mounts {
            mounted_by
                .entry(mount.layer.as_str())
                .or_default()
                .insert(manifest.name.as_str());
        }
    }
    for (layer, by) in mounted_by {
        let def = &layers[layer];
        match &def.shared {
            None if by.len() > 1 => {
                return err(format!(
                    "layer `{layer}` is mounted by {} but declares no `shared` list: a layer \
                     belongs to one collaboration unless its definition says \
                     `shared = [..]` naming every collaboration that mounts it",
                    list(&by)
                ))
            }
            Some(shared) => {
                let outside: BTreeSet<&str> = by
                    .iter()
                    .copied()
                    .filter(|m| !shared.contains(*m))
                    .collect();
                if !outside.is_empty() {
                    return err(format!(
                        "layer `{layer}` is shared with {} only, and is mounted by {} too",
                        list(&shared.iter().map(String::as_str).collect()),
                        list(&outside)
                    ));
                }
            }
            None => {}
        }
    }
    Ok(())
}

fn list(names: &BTreeSet<&str>) -> String {
    names
        .iter()
        .map(|n| format!("`{n}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The host's own options, beside `layers/` in the config dir. Optional: without it every
/// option has its default.
pub const HOST_FILE: &str = "layers.toml";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HostToml {
    #[serde(rename = "match")]
    matching: Option<MatchToml>,
    onboard: Option<OnboardToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OnboardToml {
    grants: Option<String>,
    controller: Option<String>,
    controller_key: Option<String>,
    trust_dir: Option<String>,
    trust_reload: Option<String>,
}

/// When the door this host's enrollment feeds reads its client trust list
/// ([`OnboardConfig::trust_reload`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustReload {
    /// At every handshake (cli's `serve quic://` since ledger #1825): a certificate written
    /// to the trust directory is trusted at its next connection, so applying an entitlement
    /// needs no restart. The default.
    Live,
    /// Only when the door starts: after the host applies an entitlement, the door must be
    /// restarted before the new certificate can connect, and the host says so (in its log and
    /// at [`crate::onboard::STATUS`]). It never restarts anything itself.
    Start,
}

/// The host's onboarding (ledger #2009): the `[onboard]` table of [`HOST_FILE`].
///
/// ```toml
/// # layers.toml
/// [onboard]
/// grants         = "grants"             # the channel layer entitlements are said on
/// controller     = "brian"              # the ONE participant who may say on it
/// controller_key = "controller.pub.pem" # the controller's Ed25519 public key (SPKI PEM),
///                                       # a file beside layers.toml
/// trust_dir      = "/home/door/.ikigai/quic/clients"  # where an entitled certificate goes
/// trust_reload   = "live"               # or "start": the door reads trust_dir only at start
/// ```
///
/// Without the table nothing is applied. With it, `grants`, `controller`, `controller_key` and
/// `trust_dir` are required, because each names something the host would otherwise have to
/// guess: the layer it trusts, who alone may say on it, the key that decides, and the directory
/// the door trusts certificates from.
///
/// **Only the controller says on the grants layer**, held three ways: the host refuses to open
/// when a manifest grants `say` on it to anyone else, refuses an entitlement that grants more
/// than `read` or `feed` on it, and refuses a `say` on it from any other principal. A node
/// reads its own entitlement there without verifying the signature, so this is what it relies
/// on.
#[derive(Clone, Debug)]
pub struct OnboardConfig {
    /// The channel layer whose entries are entitlements.
    pub grants: String,
    /// The one participant who may say on it, by IRI.
    pub controller: String,
    /// The controller's public key, as the SPKI PEM the file holds.
    pub controller_key: String,
    /// The directory an entitled certificate is written to (`{fingerprint}.crt`). Relative
    /// paths are resolved against the config directory.
    pub trust_dir: PathBuf,
    /// When the door reads `trust_dir`.
    pub trust_reload: TrustReload,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MatchToml {
    budget_ms: Option<u64>,
    max_overdue: Option<usize>,
}

/// The time budget every `rd match=` on this host runs within, from the `[match]` table of
/// [`HOST_FILE`] (ledger #1362):
///
/// ```toml
/// # layers.toml
/// [match]
/// budget_ms   = 2000   # how long one template may run before its caller gets a Timeout
/// max_overdue = 1      # how many may run on past that, host-wide, before rd match= refuses
/// ```
///
/// Absent (the file, the table or a key), each is ikigai-store's default: a 5 s budget, and
/// a quarter of this machine's cores overdue (at least one). The host builds ONE
/// [`EvaluationBudget`](ikigai_store::budget::EvaluationBudget) from this and hands it to
/// every tuplespace layer, so `max_overdue` caps the cores the templates of ALL its layers
/// can hold, not each layer's.
///
/// A template is evaluated under the host's own authority, never the caller's, so a
/// `urn:cap:store:budget:<ms>` grant a participant holds does not raise it: `budget_ms` is
/// what every caller gets. Zero for either is refused (no template could run, or none could
/// be abandoned), never rounded up.
pub fn load_match_budget(dir: &Path) -> Result<TimeBudget, ConfigError> {
    let path = dir.join(HOST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TimeBudget::default()),
        Err(e) => return err(format!("read {}: {e}", path.display())),
    };
    let at = path.display();
    let parsed: HostToml = toml::from_str(&text).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let Some(matching) = parsed.matching else {
        return Ok(TimeBudget::default());
    };
    let mut budget = TimeBudget::default();
    match matching.budget_ms {
        Some(0) => {
            return err(format!(
                "{at}: `match.budget_ms` is 0, so no template could ever answer; give a \
                 positive number of milliseconds, or leave it out for the default"
            ))
        }
        Some(ms) => {
            budget = TimeBudget::new(std::time::Duration::from_millis(ms))
                .with_max_overdue(budget.max_overdue())
        }
        None => {}
    }
    match matching.max_overdue {
        Some(0) => {
            return err(format!(
                "{at}: `match.max_overdue` is 0; the least is 1 (one template still running \
                 after its caller was answered refuses the next until it ends)"
            ))
        }
        Some(n) => budget = budget.with_max_overdue(n),
        None => {}
    }
    Ok(budget)
}

/// The `[onboard]` table of [`HOST_FILE`] under `dir`, checked against `layers`: `None` when
/// there is no such table. See [`OnboardConfig`].
pub fn load_onboard(
    dir: &Path,
    layers: &BTreeMap<String, LayerDef>,
) -> Result<Option<OnboardConfig>, ConfigError> {
    let path = dir.join(HOST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return err(format!("read {}: {e}", path.display())),
    };
    let at = path.display();
    let parsed: HostToml = toml::from_str(&text).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let Some(onboard) = parsed.onboard else {
        return Ok(None);
    };
    let required = |value: Option<String>, key: &str, what: &str| {
        value
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| ConfigError(format!("{at}: `onboard.{key}` is required: {what}")))
    };
    let grants = required(
        onboard.grants,
        "grants",
        "the channel layer whose entries are entitlements",
    )?;
    match layers.get(&grants) {
        Some(def) if def.model == Model::Channel && !def.lobby => {}
        Some(_) => {
            return err(format!(
                "{at}: `onboard.grants` names `{grants}`, which is not a channel (or is a \
                 lobby): an entitlement is an entry the controller says"
            ))
        }
        None => {
            return err(format!(
                "{at}: `onboard.grants` names `{grants}`, which no definition defines"
            ))
        }
    }
    let controller = required(
        onboard.controller,
        "controller",
        "the one participant who may say on the grants layer (a manifest key: `brian`, or an IRI)",
    )?;
    let controller = Participant::from_manifest_key(&controller)
        .map_err(|e| ConfigError(format!("{at}: `onboard.controller`: {e}")))?
        .iri()
        .to_string();
    let key_file = required(
        onboard.controller_key,
        "controller_key",
        "the file (beside layers.toml) holding the controller's Ed25519 public key, SPKI PEM",
    )?;
    if !is_plain_file_name(&key_file) {
        return err(format!(
            "{at}: `onboard.controller_key` is `{key_file}`; it names a file beside \
             layers.toml, not a path"
        ));
    }
    let controller_key = read(&dir.join(&key_file))?;
    crate::onboard::check_public_key_pem(&controller_key)
        .map_err(|e| ConfigError(format!("{at}: `onboard.controller_key` `{key_file}`: {e}")))?;
    let trust_dir = required(
        onboard.trust_dir,
        "trust_dir",
        "the directory the door trusts client certificates from",
    )?;
    let trust_dir = dir.join(trust_dir);
    let trust_reload = match onboard.trust_reload.as_deref() {
        None | Some("live") => TrustReload::Live,
        Some("start") => TrustReload::Start,
        Some(other) => {
            return err(format!(
                "{at}: `onboard.trust_reload` is `{other}`; it is `live` (the door reads its \
                 trust list at every handshake) or `start` (only when it starts)"
            ))
        }
    };
    Ok(Some(OnboardConfig {
        grants,
        controller,
        controller_key,
        trust_dir,
        trust_reload,
    }))
}

/// The file the host writes what its verified entitlements grant into (ledger #2009), beside
/// the manifests: the grants generator reads it as a second source of grants, so the operator
/// running `ikigai-layers grants` by hand derives exactly what the host's apply step derived.
/// Written whole by [`crate::onboard`], never by hand.
pub const ENTITLEMENTS_FILE: &str = "entitlements.toml";

/// One participant a verified entitlement grants, as [`ENTITLEMENTS_FILE`] records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entitled {
    /// Who.
    pub participant: Participant,
    /// Its certificate's fingerprint, folded.
    pub fingerprint: String,
    /// The entitlement entry it was applied from.
    pub entitlement: String,
    /// Each layer it may use, and the verbs.
    pub grants: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntitlementsToml {
    #[serde(default)]
    entitled: Vec<EntitledToml>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntitledToml {
    participant: String,
    fingerprint: String,
    entitlement: String,
    grants: Vec<String>,
}

/// Parse `urn:cap:layer:{verb}:{layer}` into (layer, verb).
///
/// ```
/// use ikigai_layers::config::parse_layer_cap;
///
/// assert_eq!(parse_layer_cap("urn:cap:layer:read:tasks"), Some(("tasks", "read")));
/// assert_eq!(parse_layer_cap("urn:cap:layer:read:Tasks"), None);
/// assert_eq!(parse_layer_cap("urn:cap:collab:read:edge"), None);
/// ```
pub fn parse_layer_cap(cap: &str) -> Option<(&str, &str)> {
    let rest = cap.strip_prefix("urn:cap:layer:")?;
    let (verb, layer) = rest.split_once(':')?;
    let known = matches!(verb, "read" | "feed" | "say" | "out" | "take");
    (known && is_layer_name(layer)).then_some((layer, verb))
}

/// Check one entitled grant set against the layers and manifests: each layer defined, each
/// verb one its model has, and each layer mounted by some collaboration (a grant on a layer no
/// manifest mounts would belong to no collaboration; creating layers is ledger #2010).
pub fn check_entitled_grants(
    grants: &BTreeMap<String, BTreeSet<String>>,
    layers: &BTreeMap<String, LayerDef>,
    collabs: &BTreeMap<String, Manifest>,
) -> Result<(), String> {
    for (layer, verbs) in grants {
        let Some(def) = layers.get(layer) else {
            return Err(format!("layer `{layer}`, which no definition defines"));
        };
        if let Some(verb) = verbs
            .iter()
            .find(|v| !def.model.verbs().contains(&v.as_str()))
        {
            return Err(format!(
                "`{verb}` on `{layer}`, a {} layer, whose verbs are {}",
                def.model.as_str(),
                def.model.verbs().join(", ")
            ));
        }
        if !collabs
            .values()
            .any(|m| m.mounts.iter().any(|mount| &mount.layer == layer))
        {
            return Err(format!(
                "layer `{layer}`, which no collaboration mounts (a grant belongs to a \
                 collaboration; creating layers is ledger #2010)"
            ));
        }
    }
    Ok(())
}

/// Read [`ENTITLEMENTS_FILE`] under `dir`, checked against the layers, the participants and
/// the manifests. Absent is empty.
pub fn load_entitled(
    dir: &Path,
    layers: &BTreeMap<String, LayerDef>,
    participants: &Participants,
    collabs: &BTreeMap<String, Manifest>,
) -> Result<Vec<Entitled>, ConfigError> {
    let path = dir.join(ENTITLEMENTS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return err(format!("read {}: {e}", path.display())),
    };
    let at = path.display();
    let parsed: EntitlementsToml =
        toml::from_str(&text).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let mut entitled = Vec::new();
    for item in parsed.entitled {
        let participant =
            Participant::parse(&item.participant).map_err(|e| ConfigError(format!("{at}: {e}")))?;
        if !participants.names(participant.iri()) {
            return err(format!(
                "{at}: {} is no participant in participants.ttl",
                participant.iri()
            ));
        }
        let fingerprint = crate::name::fold_fingerprint(&item.fingerprint).ok_or_else(|| {
            ConfigError(format!(
                "{at}: `{}` is not a SHA-256 fingerprint",
                item.fingerprint
            ))
        })?;
        let mut grants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for cap in &item.grants {
            let (layer, verb) = parse_layer_cap(cap).ok_or_else(|| {
                ConfigError(format!(
                    "{at}: `{cap}` is not a layer grant (urn:cap:layer:{{verb}}:{{layer}})"
                ))
            })?;
            grants
                .entry(layer.to_string())
                .or_default()
                .insert(verb.to_string());
        }
        check_entitled_grants(&grants, layers, collabs)
            .map_err(|e| ConfigError(format!("{at}: {} is granted {e}", participant.iri())))?;
        entitled.push(Entitled {
            participant,
            fingerprint,
            entitlement: item.entitlement,
            grants,
        });
    }
    Ok(entitled)
}

/// Everything in one directory, checked.
#[derive(Clone, Debug)]
pub struct Config {
    /// The directory it was read from.
    pub dir: PathBuf,
    /// The layers, by name.
    pub layers: BTreeMap<String, LayerDef>,
    /// The participants graph.
    pub participants: Participants,
    /// The collaborations, by name.
    pub collabs: BTreeMap<String, Manifest>,
    /// What verified entitlements grant ([`ENTITLEMENTS_FILE`]), beside the manifests.
    pub entitled: Vec<Entitled>,
}

impl Config {
    /// Read and check the whole configuration under `dir`.
    pub fn load(dir: impl AsRef<Path>) -> Result<Config, ConfigError> {
        let dir = dir.as_ref().to_path_buf();
        let layers = load_layers(&dir)?;
        let participants = load_participants(&dir)?;
        let collabs = load_collabs(&dir, &layers, &participants)?;
        let entitled = load_entitled(&dir, &layers, &participants, &collabs)?;
        Ok(Config {
            dir,
            layers,
            participants,
            collabs,
            entitled,
        })
    }
}
