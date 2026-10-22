//! The enrollment generator: a door's `grants.json` and `clients.json`, DERIVED from the
//! manifests, so the manifest is the one thing an operator edits.
//!
//! For every participant some manifest grants anything:
//!
//! - one grant, named by the participant's slug, whose scopes are the union over every
//!   manifest naming it: `urn:cap:layer:{verb}:{layer}` per verb it holds on each layer,
//!   and `urn:cap:collab:read:{name}` for each manifest it appears in;
//! - one `clients.json` entry per certificate fingerprint, mapping it to that grant, with
//!   `principal` = the participant's IRI (what the door mints, and so what every entry it
//!   writes is attributed to).
//!
//! Fingerprints come from `enrollment.toml` (a participant's name, as a manifest writes
//! it, to one fingerprint or a list), from the participant's own name when it is a host
//! client (`urn:iki:host:{host}:client:<fingerprint>`), or from a verified entitlement the
//! host applied ([`crate::config::ENTITLEMENTS_FILE`]). Nothing else.
//!
//! **An entitlement grants what a manifest line would** (ledger #2009): for each layer it
//! names, `urn:cap:layer:{verb}:{layer}` per verb, and `urn:cap:collab:read:{name}` for each
//! collaboration that mounts the layer, unioned into the participant's one grant.
//!
//! **Isolation is structural here.** The generator reads only manifests that
//! [`crate::config::load_collabs`] accepted, and that refuses a manifest mounting a layer
//! another manifest mounts unless the layer is declared shared with both. So no grant this
//! writes can reach a layer the participant's own collaborations do not mount. And it
//! writes no `default` grant: a certificate it did not enroll is refused by the door.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::config::{Config, ConfigError};
use crate::name::{collab_cap, fold_fingerprint, layer_cap, Participant};

/// One `clients.json` entry, in the object form cli's door reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Client {
    /// The grant (in `grants.json`) this certificate is enrolled under.
    pub grant: String,
    /// The principal the door mints for it: the participant's IRI.
    pub principal: String,
    /// For humans reading the file and the door's log.
    pub label: String,
}

/// What the generator derived.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Enrollment {
    /// Grant name → scopes, sorted.
    pub grants: BTreeMap<String, Vec<String>>,
    /// Fingerprint → client entry.
    pub clients: BTreeMap<String, Client>,
    /// Participants holding a grant but no certificate yet (no client entry written).
    pub unenrolled: Vec<String>,
    /// Participants with a certificate but no grant in any manifest (no client entry
    /// written: the door would refuse it anyway, and an entry naming no grant is noise).
    pub idle: Vec<String>,
}

/// Read `enrollment.toml` under `dir`: participant (as a manifest names it) → fingerprint
/// or fingerprints, with every fingerprint `config`'s entitlements record added. Absent is
/// no hand-enrolled certificate.
pub fn load_fingerprints(
    dir: &Path,
    config: &Config,
) -> Result<BTreeMap<String, BTreeSet<String>>, ConfigError> {
    let path = dir.join("enrollment.toml");
    let at = path.display();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        // Absent is no hand-enrolled certificate; entitled ones are still read below.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(ConfigError(format!("read {at}: {e}"))),
    };
    let table: toml::Table =
        toml::from_str(&text).map_err(|e| ConfigError(format!("{at}: {e}")))?;
    let mut fingerprints: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (key, value) in table {
        let participant =
            Participant::from_manifest_key(&key).map_err(|e| ConfigError(format!("{at}: {e}")))?;
        if !config.participants.names(participant.iri()) {
            return Err(ConfigError(format!(
                "{at}: `{key}` is no participant in participants.ttl"
            )));
        }
        let raw: Vec<String> = match value {
            toml::Value::String(s) => vec![s],
            toml::Value::Array(items) => items
                .into_iter()
                .map(|i| match i {
                    toml::Value::String(s) => Ok(s),
                    _ => Err(ConfigError(format!("{at}: `{key}` lists a non-string"))),
                })
                .collect::<Result<_, _>>()?,
            _ => {
                return Err(ConfigError(format!(
                    "{at}: `{key}` is a fingerprint or a list of them"
                )))
            }
        };
        let set = fingerprints
            .entry(participant.iri().to_string())
            .or_default();
        for fp in raw {
            set.insert(fold_fingerprint(&fp).ok_or_else(|| {
                ConfigError(format!(
                    "{at}: `{key}`: `{fp}` is not a SHA-256 fingerprint (64 hex digits)"
                ))
            })?);
        }
    }
    for entitled in &config.entitled {
        fingerprints
            .entry(entitled.participant.iri().to_string())
            .or_default()
            .insert(entitled.fingerprint.clone());
    }
    Ok(fingerprints)
}

/// Derive the enrollment from a checked configuration and the fingerprints.
pub fn generate(
    config: &Config,
    fingerprints: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Enrollment, ConfigError> {
    let mut scopes: BTreeMap<Participant, BTreeSet<String>> = BTreeMap::new();
    for manifest in config.collabs.values() {
        for mount in &manifest.mounts {
            for (participant, verbs) in &mount.grants {
                let held = scopes.entry(participant.clone()).or_default();
                held.insert(collab_cap(&manifest.name));
                held.extend(verbs.iter().map(|verb| layer_cap(verb, &mount.layer)));
            }
        }
    }
    for entitled in &config.entitled {
        let held = scopes.entry(entitled.participant.clone()).or_default();
        for (layer, verbs) in &entitled.grants {
            held.extend(verbs.iter().map(|verb| layer_cap(verb, layer)));
            for manifest in config.collabs.values() {
                if manifest.mounts.iter().any(|m| &m.layer == layer) {
                    held.insert(collab_cap(&manifest.name));
                }
            }
        }
    }
    let mut enrollment = Enrollment::default();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for (participant, held) in &scopes {
        enrollment.grants.insert(
            participant.slug().to_string(),
            held.iter().cloned().collect(),
        );
        let mut certs: BTreeSet<String> = fingerprints
            .get(participant.iri())
            .cloned()
            .unwrap_or_default();
        certs.extend(participant.fingerprint().map(str::to_string));
        if certs.is_empty() {
            enrollment.unenrolled.push(participant.iri().to_string());
        }
        let label = config.participants.members[participant.iri()]
            .label
            .clone()
            .unwrap_or_else(|| participant.slug().to_string());
        for fp in certs {
            if let Some(other) = owner.insert(fp.clone(), participant.iri().to_string()) {
                return Err(ConfigError(format!(
                    "certificate {fp} is enrolled for both {other} and {}: one certificate \
                     names one actor",
                    participant.iri()
                )));
            }
            enrollment.clients.insert(
                fp,
                Client {
                    grant: participant.slug().to_string(),
                    principal: participant.iri().to_string(),
                    label: label.clone(),
                },
            );
        }
    }
    for (iri, certs) in fingerprints {
        if scopes.keys().any(|p| p.iri() == iri) {
            continue;
        }
        for fp in certs {
            if let Some(other) = owner.insert(fp.clone(), iri.clone()) {
                return Err(ConfigError(format!(
                    "certificate {fp} is enrolled for both {other} and {iri}"
                )));
            }
        }
        enrollment.idle.push(iri.clone());
    }
    Ok(enrollment)
}

impl Enrollment {
    /// `grants.json`.
    pub fn grants_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(&self.grants).expect("a map of strings");
        text.push('\n');
        text
    }

    /// `clients.json`, with no `default`: an unenrolled certificate is refused.
    pub fn clients_json(&self) -> String {
        let mut text =
            serde_json::to_string_pretty(&serde_json::json!({ "clients": self.clients }))
                .expect("a map of strings");
        text.push('\n');
        text
    }

    /// Write both files into `dir`, each atomically (a temporary file renamed over the
    /// old), so a door re-reading them per connection never sees half of one.
    pub fn write(&self, dir: &Path) -> Result<(), String> {
        for (name, text) in [
            ("grants.json", self.grants_json()),
            ("clients.json", self.clients_json()),
        ] {
            let path = dir.join(name);
            let tmp = dir.join(format!(".{name}.tmp"));
            std::fs::write(&tmp, text)
                .and_then(|()| std::fs::rename(&tmp, &path))
                .map_err(|e| format!("write {}: {e}", path.display()))?;
        }
        Ok(())
    }
}
