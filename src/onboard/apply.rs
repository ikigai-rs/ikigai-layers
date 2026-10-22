//! Applying verified entitlements to the door's enrollment, and the lobby's rate.
//!
//! **The record is the grants layer.** What the host applies is recomputed from every entry
//! of that layer, in feed order, each time it changes (and once when the host opens), so
//! applying is idempotent by construction: the same entries give the same files, and an
//! entry seen twice is applied once. Verdicts on entries are cached by IRI, because an entry
//! never changes, so a signature is verified once per host.
//!
//! **Per fingerprint, the newest verified entitlement wins, and it must say so.** An
//! entitlement for a fingerprint that already has one must `ik:supersedes` that one, or it is
//! refused: a copy of an old signed entitlement said again (a replay) supersedes nothing
//! current and so changes nothing. A superseding entitlement with no `onb:layerGrant` is a
//! revocation.
//!
//! **What it writes**, all in the host's config directory but the certificate:
//!
//! - `participants.ttl`: the entitled participant, appended when the graph does not name it
//!   (an `ik:Machine` labeled by its name). Never removed: a participant with no grant can do
//!   nothing, and the entries it wrote stay attributed to a name the graph still explains;
//! - [`ENTITLEMENTS_FILE`]: every effective entitlement, written whole;
//! - `grants.json` and `clients.json`, by the grants generator ([`crate::grants`]) over the
//!   manifests and that file, exactly as `ikigai-layers grants` derives them;
//! - `{trust_dir}/{fingerprint}.crt`: the certificate, from the lobby request the
//!   entitlement names; removed when the entitlement is revoked or expires.
//!
//! Only when what is effective differs from [`ENTITLEMENTS_FILE`] are the config files
//! written, so a host opening over an unchanged grants layer rewrites nothing.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Fallback, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Result as CoreResult, Space, Time, Verb,
};
use oxrdf::Triple;
use serde::Serialize;

use super::{
    fingerprint_of_pem, one_literal, signing_bytes, CERTIFICATE, ENTITLEMENT, EXPIRES, FINGERPRINT,
    LAYER_GRANT, LOBBY_SAYS_PER_FINGERPRINT, LOBBY_SAYS_TOTAL, LOBBY_WINDOW_MILLIS, PRINCIPAL,
    REQUEST, SIGNATURE,
};
use crate::at_rest;
use crate::config::{self, parse_layer_cap, Config, OnboardConfig, TrustReload, ENTITLEMENTS_FILE};
use crate::grants::{generate, load_fingerprints};
use crate::host::Host;
use crate::name::{fold_fingerprint, Participant, PARTICIPANT_PREFIX};
use crate::rdf::{self, object_iri};
use crate::time::xsd_date_time;

/// Where the controller's public key is served in the host's verification kernel, which no
/// door binds.
const CONTROLLER_KEY: &str = "urn:layer:onboard:controller-key";
const VERIFY: &str = "urn:sign:verify";

/// What the host decided about one entitlement entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", rename_all = "lowercase")]
pub enum Verdict {
    /// Verified and in force.
    Applied,
    /// Verified, a revocation of the entitlement it supersedes.
    Revoked,
    /// Verified and applied once, then superseded by `by`.
    Superseded {
        /// The entitlement that superseded it.
        by: String,
    },
    /// Verified, but past its `onb:expires`: nothing it granted is in force.
    Expired,
    /// Not applied, and why: a signature that does not verify against the controller key, a
    /// field missing, a replay, a grant on a layer the host cannot grant.
    Refused {
        /// Why.
        reason: String,
    },
}

/// One entitlement entry, as the host decided it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Record {
    /// The entry.
    pub entry: String,
    /// Its place in the grants layer's feed.
    pub seq: u64,
    /// What the host decided.
    #[serde(flatten)]
    pub verdict: Verdict,
}

/// An entitlement in force.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Effective {
    /// The certificate it enrolls.
    pub fingerprint: String,
    /// The participant the certificate acts as.
    pub participant: String,
    /// The entry it was applied from.
    pub entitlement: String,
    /// What it grants, `urn:cap:layer:{verb}:{layer}`, sorted.
    pub grants: Vec<String>,
    #[serde(skip)]
    certificate: String,
}

/// The host's onboarding status ([`super::STATUS`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    /// Entitlement entries seen on the grants layer.
    pub seen: usize,
    /// Of those, applied at some time (in force, revoking, or superseded since).
    pub applied: usize,
    /// Of those, refused.
    pub refused: usize,
    /// Every entry, in feed order.
    pub records: Vec<Record>,
    /// What is in force, by fingerprint.
    pub effective: Vec<Effective>,
    /// Whether the door must be restarted before what was applied takes effect
    /// (`onboard.trust_reload = "start"`). The host never restarts anything itself.
    pub restart_pending: bool,
    /// The last apply's failure to write its files, if it failed: what is effective above is
    /// then NOT what the door reads.
    pub error: Option<String>,
}

impl Status {
    /// The status as plain text: one line per fact, then one per entry.
    pub fn text(&self) -> String {
        let mut out = format!(
            "seen {}\napplied {}\nrefused {}\nrestart {}\n",
            self.seen,
            self.applied,
            self.refused,
            if self.restart_pending {
                "pending: restart the door"
            } else {
                "not needed"
            }
        );
        if let Some(e) = &self.error {
            out.push_str(&format!("error {e}\n"));
        }
        for e in &self.effective {
            out.push_str(&format!(
                "effective {} {} {} {}\n",
                e.fingerprint,
                e.participant,
                e.entitlement,
                e.grants.join(" ")
            ));
        }
        for r in &self.records {
            out.push_str(&format!(
                "entry {} {} {}\n",
                r.seq,
                r.entry,
                describe(&r.verdict)
            ));
        }
        out
    }
}

fn describe(verdict: &Verdict) -> String {
    match verdict {
        Verdict::Applied => "applied".into(),
        Verdict::Revoked => "revoked".into(),
        Verdict::Superseded { by } => format!("superseded by {by}"),
        Verdict::Expired => "expired".into(),
        Verdict::Refused { reason } => format!("refused: {reason}"),
    }
}

/// An entitlement whose signature verified, with its fields.
#[derive(Clone, Debug)]
struct Verified {
    fingerprint: String,
    participant: Participant,
    request: Option<String>,
    grants: BTreeMap<String, BTreeSet<String>>,
    expires: Option<String>,
    supersedes: BTreeSet<String>,
}

/// One entry of the grants layer, as read for applying.
struct Entry {
    iri: String,
    seq: u64,
    supersedes: BTreeSet<String>,
    triples: Result<Vec<Triple>, String>,
}

/// The host's onboarding: its configuration, its own verification kernel, and what it has
/// decided so far.
pub(crate) struct Onboard {
    pub(crate) config: OnboardConfig,
    verifier: Kernel,
    /// Serializes applying, across the verification sub-requests (awaits).
    apply: futures::lock::Mutex<Book>,
    status: Mutex<Status>,
}

#[derive(Default)]
struct Book {
    /// Each entry's verification, by IRI: an entry never changes.
    verified: HashMap<String, Result<Verified, String>>,
    /// The entries already written to the log.
    logged: HashSet<String>,
}

impl Onboard {
    pub(crate) fn new(config: OnboardConfig) -> Onboard {
        let key = EndpointSpace::new().bind(
            Exact::new(CONTROLLER_KEY),
            KeyEndpoint(config.controller_key.clone().into_bytes()),
        );
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_sign::space()), Arc::new(key)];
        Onboard {
            config,
            verifier: Kernel::new(Arc::new(Fallback::new(spaces))),
            apply: futures::lock::Mutex::new(Book::default()),
            status: Mutex::new(Status::default()),
        }
    }

    pub(crate) fn status(&self) -> Status {
        self.status.lock().expect("status lock").clone()
    }

    /// Verify `bytes` against the signature graph `sig` under the controller key.
    async fn verify(&self, bytes: Vec<u8>, sig: &str) -> Result<(), String> {
        let request = Request::new(Verb::Source, Iri::parse(VERIFY).expect("a constant IRI"))
            .with_arg("in", ArgRef::Inline(bytes))
            .with_arg("sig", ArgRef::Inline(sig.as_bytes().to_vec()))
            .with_arg("key", ArgRef::Inline(CONTROLLER_KEY.as_bytes().to_vec()));
        let answer = self
            .verifier
            .issue(request, &Capability::scoped(Vec::<String>::new()))
            .await
            .map_err(|e| format!("the signature does not verify ({e})"))?;
        let text = String::from_utf8_lossy(&answer.bytes);
        if text.starts_with("valid:") {
            Ok(())
        } else {
            Err(format!(
                "the signature does not verify against the controller key ({})",
                text.trim()
            ))
        }
    }

    async fn check(
        &self,
        triples: &[Triple],
        iri: &str,
        supersedes: &BTreeSet<String>,
    ) -> Result<Verified, String> {
        let types: BTreeSet<&str> = triples
            .iter()
            .filter(|t| t.predicate.as_str() == rdf::RDF_TYPE)
            .filter_map(|t| object_iri(&t.object))
            .collect();
        if !types.contains(ENTITLEMENT) {
            return Err("not an onb:Entitlement".into());
        }
        let signature = one_literal(triples, SIGNATURE)
            .ok_or("no single onb:signature: an entitlement is signed by the controller")?;
        self.verify(signing_bytes(triples, iri), signature).await?;
        let fingerprint = one_literal(triples, FINGERPRINT)
            .and_then(fold_fingerprint)
            .ok_or("no single onb:fingerprint of 64 hex digits")?;
        let principal = one_iri(triples, PRINCIPAL).ok_or("no single onb:principal")?;
        let participant =
            Participant::parse(principal).map_err(|e| format!("onb:principal {e}"))?;
        if !principal.starts_with(PARTICIPANT_PREFIX) {
            return Err(format!(
                "onb:principal {principal}: an entitled node is {PARTICIPANT_PREFIX}{{name}}"
            ));
        }
        let mut grants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for t in triples
            .iter()
            .filter(|t| t.predicate.as_str() == LAYER_GRANT)
        {
            let cap = object_iri(&t.object).ok_or("an onb:layerGrant that is not an IRI")?;
            let (layer, verb) = parse_layer_cap(cap).ok_or_else(|| {
                format!("onb:layerGrant {cap} is not urn:cap:layer:{{verb}}:{{layer}}")
            })?;
            grants
                .entry(layer.to_string())
                .or_default()
                .insert(verb.to_string());
        }
        let expires = match triples
            .iter()
            .filter(|t| t.predicate.as_str() == EXPIRES)
            .count()
        {
            0 => None,
            1 => Some(
                one_literal(triples, EXPIRES)
                    .ok_or("onb:expires is not a literal")?
                    .to_string(),
            ),
            _ => return Err("more than one onb:expires".into()),
        };
        Ok(Verified {
            fingerprint,
            participant,
            request: one_iri(triples, REQUEST).map(str::to_string),
            grants,
            expires,
            supersedes: supersedes.clone(),
        })
    }

    /// Apply every entitlement on the grants layer: recompute what is in force, and write the
    /// files when it changed. `now` decides expiry. Never fails: what went wrong is in the
    /// status, and on the host's log (stderr).
    pub(crate) async fn apply(&self, host: &Host, now: Time) {
        let mut book = self.apply.lock().await;
        let Some(layer) = host.layer(&self.config.grants) else {
            return;
        };
        // Read every entry first: the channel's lock is a std lock, never held across an await.
        let entries: Vec<Entry> = {
            let channel = layer.channel().read().expect("channel lock");
            channel
                .entries()
                .iter()
                .map(|meta| {
                    let triples = channel
                        .turtle(meta)
                        .and_then(|text| rdf::parse_turtle(&text));
                    Entry {
                        iri: meta.iri.clone(),
                        seq: meta.seq,
                        supersedes: meta.supersedes.clone(),
                        triples,
                    }
                })
                .collect()
        };
        for entry in &entries {
            if book.verified.contains_key(&entry.iri) {
                continue;
            }
            let verdict = match &entry.triples {
                Ok(triples) => self.check(triples, &entry.iri, &entry.supersedes).await,
                Err(e) => Err(format!("the entry does not read: {e}")),
            };
            book.verified.insert(entry.iri.clone(), verdict);
        }
        let mut status = fold(&self.config, host, &entries, &book.verified, now);
        let previous = self.status();
        status.restart_pending = previous.restart_pending;
        match self.write(host, &status.effective, &entries, &book.verified) {
            Ok(changed) => {
                if changed && self.config.trust_reload == TrustReload::Start {
                    status.restart_pending = true;
                    eprintln!(
                        "ikigai-layers: onboard: entitlements applied; RESTART THE DOOR: it \
                         reads its trust list only when it starts (onboard.trust_reload = \
                         \"start\")"
                    );
                }
            }
            Err(e) => {
                eprintln!("ikigai-layers: onboard: applying entitlements failed: {e}");
                status.error = Some(e);
            }
        }
        for record in &status.records {
            if book.logged.insert(record.entry.clone()) {
                eprintln!(
                    "ikigai-layers: onboard: {} {}",
                    record.entry,
                    describe(&record.verdict)
                );
            }
        }
        *self.status.lock().expect("status lock") = status;
    }

    /// Write what is effective. Returns whether anything changed.
    fn write(
        &self,
        host: &Host,
        effective: &[Effective],
        entries: &[Entry],
        verified: &HashMap<String, Result<Verified, String>>,
    ) -> Result<bool, String> {
        let dir = host.config_dir();
        let current = Config::load(dir).map_err(|e| e.to_string())?;
        let desired: Vec<config::Entitled> = effective
            .iter()
            .map(|e| config::Entitled {
                participant: Participant::parse(&e.participant).expect("checked when verified"),
                fingerprint: e.fingerprint.clone(),
                entitlement: e.entitlement.clone(),
                grants: grants_of(&e.grants),
            })
            .collect();
        let mut changed = false;
        if current.entitled != desired {
            changed = true;
            append_participants(dir, &current, effective)?;
            let path = dir.join(ENTITLEMENTS_FILE);
            let before = std::fs::read_to_string(&path).ok();
            write_atomic(&path, &entitlements_toml(effective))?;
            let regenerated = Config::load(dir)
                .and_then(|config| {
                    let fingerprints = load_fingerprints(dir, &config)?;
                    generate(&config, &fingerprints)
                })
                .map_err(|e| e.to_string())
                .and_then(|enrollment| enrollment.write(dir));
            if let Err(e) = regenerated {
                // Put the last good file back, so the generator still derives what the door
                // reads, and say why nothing was applied.
                match before {
                    Some(text) => write_atomic(&path, &text)?,
                    None => std::fs::remove_file(&path)
                        .map_err(|e| format!("remove {}: {e}", path.display()))?,
                }
                return Err(format!("the grants generator refused: {e}"));
            }
        }
        // The trust directory: every effective certificate present, every one this host ever
        // entitled and no longer does removed. Nothing else in it is touched.
        let trust = &self.config.trust_dir;
        at_rest::private_dir(trust).map_err(|e| format!("create {}: {e}", trust.display()))?;
        let keep: BTreeSet<&str> = effective.iter().map(|e| e.fingerprint.as_str()).collect();
        for e in effective {
            let path = trust.join(format!("{}.crt", e.fingerprint));
            if std::fs::read_to_string(&path).ok().as_deref() != Some(e.certificate.as_str()) {
                write_atomic(&path, &e.certificate)?;
                changed = true;
            }
        }
        let ever: BTreeSet<&str> = entries
            .iter()
            .filter_map(|entry| verified.get(&entry.iri)?.as_ref().ok())
            .map(|v| v.fingerprint.as_str())
            .collect();
        for fp in ever.difference(&keep) {
            let path = trust.join(format!("{fp}.crt"));
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("remove {}: {e}", path.display()))?;
                changed = true;
            }
        }
        Ok(changed)
    }
}

/// Recompute what is in force from every entry, in feed order.
fn fold(
    onboard: &OnboardConfig,
    host: &Host,
    entries: &[Entry],
    verified: &HashMap<String, Result<Verified, String>>,
    now: Time,
) -> Status {
    let mut records: Vec<Record> = Vec::new();
    // Per fingerprint: the current entitlement (in force, revoking or expired) and, when it
    // grants, what is in force.
    let mut current: BTreeMap<String, String> = BTreeMap::new();
    let mut effective: BTreeMap<String, Effective> = BTreeMap::new();
    let collabs = host.collabs().ok();
    for Entry { iri, seq, .. } in entries {
        let record = |verdict| Record {
            entry: iri.clone(),
            seq: *seq,
            verdict,
        };
        let v = match verified.get(iri) {
            Some(Ok(v)) => v,
            Some(Err(reason)) => {
                records.push(record(Verdict::Refused {
                    reason: reason.clone(),
                }));
                continue;
            }
            None => continue,
        };
        // The supersession rule: an entitlement for a fingerprint with a current one must
        // supersede exactly that one; one for a new fingerprint supersedes nothing.
        let refusal = match current.get(&v.fingerprint) {
            Some(cur) if !v.supersedes.contains(cur) => Some(format!(
                "fingerprint {} is already entitled by {cur}, which this does not supersede \
                 (a replay, or a stale controller)",
                v.fingerprint
            )),
            None if !v.supersedes.is_empty() => Some(format!(
                "supersedes {}, which is not the current entitlement of fingerprint {}",
                v.supersedes.iter().cloned().collect::<Vec<_>>().join(", "),
                v.fingerprint
            )),
            _ => None,
        };
        if let Some(reason) = refusal {
            records.push(record(Verdict::Refused { reason }));
            continue;
        }
        if v.grants.is_empty() {
            if !current.contains_key(&v.fingerprint) {
                records.push(record(Verdict::Refused {
                    reason: "an entitlement with no onb:layerGrant revokes, and this supersedes \
                             nothing"
                        .into(),
                }));
                continue;
            }
            mark_superseded(&mut records, &v.supersedes, iri);
            effective.remove(&v.fingerprint);
            current.insert(v.fingerprint.clone(), iri.clone());
            records.push(record(Verdict::Revoked));
            continue;
        }
        // A grant must be one this host can make.
        let grantable = match &collabs {
            Some(collabs) => {
                let defs: BTreeMap<String, crate::config::LayerDef> = host
                    .layers()
                    .iter()
                    .map(|(n, s)| (n.clone(), s.def.clone()))
                    .collect();
                config::check_entitled_grants(&v.grants, &defs, collabs)
                    .map_err(|e| format!("grants {e}"))
            }
            None => Err("the host's manifests do not read".into()),
        };
        let grants_layer = &onboard.grants;
        let grantable = grantable.and_then(|()| match v.grants.get(grants_layer) {
            Some(verbs) if verbs.iter().any(|v| v != "read" && v != "feed") => Err(format!(
                "grants more than read and feed on `{grants_layer}`: only the controller says \
                 entitlements"
            )),
            _ => Ok(()),
        });
        let certificate = grantable.and_then(|()| certificate_of(host, v));
        let certificate = match certificate {
            Ok(pem) => pem,
            Err(reason) => {
                records.push(record(Verdict::Refused { reason }));
                continue;
            }
        };
        mark_superseded(&mut records, &v.supersedes, iri);
        current.insert(v.fingerprint.clone(), iri.clone());
        match expired(v.expires.as_deref(), now) {
            Ok(false) => {
                effective.insert(
                    v.fingerprint.clone(),
                    Effective {
                        fingerprint: v.fingerprint.clone(),
                        participant: v.participant.iri().to_string(),
                        entitlement: iri.clone(),
                        grants: with_read(&v.grants, grants_layer)
                            .iter()
                            .flat_map(|(layer, verbs)| {
                                verbs
                                    .iter()
                                    .map(move |verb| crate::name::layer_cap(verb, layer))
                            })
                            .collect(),
                        certificate,
                    },
                );
                records.push(record(Verdict::Applied));
            }
            Ok(true) => {
                effective.remove(&v.fingerprint);
                records.push(record(Verdict::Expired));
            }
            Err(reason) => {
                effective.remove(&v.fingerprint);
                records.push(record(Verdict::Refused { reason }));
            }
        }
    }
    let refused = records
        .iter()
        .filter(|r| matches!(r.verdict, Verdict::Refused { .. }))
        .count();
    Status {
        seen: records.len(),
        applied: records.len() - refused,
        refused,
        records,
        effective: effective.into_values().collect(),
        restart_pending: false,
        error: None,
    }
}

/// `grants` with `read` on the grants layer added: an entitled node reads its own entitlement
/// there, under the certificate it was entitled with.
fn with_read(
    grants: &BTreeMap<String, BTreeSet<String>>,
    grants_layer: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut grants = grants.clone();
    grants
        .entry(grants_layer.to_string())
        .or_default()
        .insert("read".into());
    grants
}

fn mark_superseded(records: &mut [Record], superseded: &BTreeSet<String>, by: &str) {
    for r in records.iter_mut().filter(|r| superseded.contains(&r.entry)) {
        if !matches!(r.verdict, Verdict::Refused { .. }) {
            r.verdict = Verdict::Superseded { by: by.to_string() };
        }
    }
}

/// Whether `expires` (an `xsd:dateTime` with a timezone) is at or before `now`.
fn expired(expires: Option<&str>, now: Time) -> Result<bool, String> {
    let Some(expires) = expires else {
        return Ok(false);
    };
    let at = oxsdatatypes::DateTime::from_str(expires)
        .map_err(|e| format!("onb:expires `{expires}` is not an xsd:dateTime: {e}"))?;
    let now = oxsdatatypes::DateTime::from_str(&xsd_date_time(now)).expect("the clock's form");
    match at.partial_cmp(&now) {
        Some(order) => Ok(order != std::cmp::Ordering::Greater),
        None => Err(format!(
            "onb:expires `{expires}` carries no timezone, so it cannot be compared with now"
        )),
    }
}

/// The certificate an entitlement enrolls, from the lobby request it names, checked again
/// against the entitlement's fingerprint.
fn certificate_of(host: &Host, v: &Verified) -> Result<String, String> {
    let request = v
        .request
        .as_deref()
        .ok_or("no onb:request: an entitlement that grants names the request it grants")?;
    let (lobby, id) = request
        .strip_prefix("urn:layer:")
        .and_then(|rest| rest.split_once(":entry:"))
        .ok_or_else(|| format!("onb:request {request} is not a layer entry"))?;
    let state = host
        .layer(lobby)
        .filter(|s| s.def.lobby)
        .ok_or_else(|| format!("onb:request {request} is not an entry of a lobby of this host"))?;
    let triples = {
        let channel = state.channel().read().expect("channel lock");
        let meta = channel
            .get(id)
            .ok_or_else(|| format!("onb:request {request}: no such entry"))?;
        channel
            .turtle(meta)
            .and_then(|text| rdf::parse_turtle(&text))
            .map_err(|e| format!("onb:request {request} does not read: {e}"))?
    };
    let stated = one_literal(&triples, FINGERPRINT);
    if stated != Some(v.fingerprint.as_str()) {
        return Err(format!(
            "onb:request {request} is for another certificate than onb:fingerprint {}",
            v.fingerprint
        ));
    }
    let pem = one_literal(&triples, CERTIFICATE)
        .ok_or_else(|| format!("onb:request {request} carries no certificate"))?;
    match fingerprint_of_pem(pem) {
        Ok(fp) if fp == v.fingerprint => Ok(pem.to_string()),
        Ok(fp) => Err(format!(
            "onb:request {request}'s certificate hashes to {fp}, not {}",
            v.fingerprint
        )),
        Err(e) => Err(format!("onb:request {request}: {e}")),
    }
}

fn one_iri<'a>(triples: &'a [Triple], predicate: &str) -> Option<&'a str> {
    let mut values = triples.iter().filter(|t| t.predicate.as_str() == predicate);
    match (values.next(), values.next()) {
        (Some(t), None) => object_iri(&t.object),
        _ => None,
    }
}

fn grants_of(caps: &[String]) -> BTreeMap<String, BTreeSet<String>> {
    let mut grants: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for cap in caps {
        if let Some((layer, verb)) = parse_layer_cap(cap) {
            grants
                .entry(layer.to_string())
                .or_default()
                .insert(verb.to_string());
        }
    }
    grants
}

fn entitlements_toml(effective: &[Effective]) -> String {
    let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
    let mut out = String::from(
        "# WRITTEN BY THE LAYER HOST from the verified entitlements on its grants layer (ledger\n\
         # #2009); the grants generator reads it beside the manifests. Do not edit: the next\n\
         # entitlement rewrites it. Its status is urn:layer:onboard:status.\n",
    );
    for e in effective {
        out.push_str(&format!(
            "\n[[entitled]]\nparticipant = {}\nfingerprint = {}\nentitlement = {}\ngrants = [{}]\n",
            quote(&e.participant),
            quote(&e.fingerprint),
            quote(&e.entitlement),
            e.grants
                .iter()
                .map(|g| quote(g))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    out
}

/// Append each entitled participant `participants.ttl` does not name yet.
fn append_participants(
    dir: &Path,
    current: &Config,
    effective: &[Effective],
) -> Result<(), String> {
    let mut added = String::new();
    let mut seen = BTreeSet::new();
    for e in effective {
        if current.participants.names(&e.participant) || !seen.insert(&e.participant) {
            continue;
        }
        let name = e.participant.trim_start_matches(PARTICIPANT_PREFIX);
        added.push_str(&format!(
            "\n# Added by the layer host from the verified entitlement <{}> (ledger #2009).\n\
             <{}> a <{}> ; <{}> {} .\n",
            e.entitlement,
            e.participant,
            rdf::ik_machine(),
            rdf::RDFS_LABEL,
            oxrdf::Literal::new_simple_literal(name)
        ));
    }
    if added.is_empty() {
        return Ok(());
    }
    let path = dir.join("participants.ttl");
    let mut text = std::fs::read_to_string(&path).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&added);
    write_atomic(&path, &text)
}

/// Write `text` to `path` through a temporary file renamed over it, so a reader never sees
/// half of it.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp: PathBuf = path.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, text)
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// The controller's public key, served to ikigai-sign's verifier in the host's own kernel.
struct KeyEndpoint(Vec<u8>);

#[async_trait]
impl Endpoint for KeyEndpoint {
    async fn invoke(&self, _inv: &Invocation<'_>) -> CoreResult<Representation> {
        Ok(Representation::new(
            ReprType::new("application/x-pem-file"),
            self.0.clone(),
        ))
    }

    fn name(&self) -> &str {
        "onboard-controller-key"
    }

    fn describe(&self) -> Description {
        Description::new("onboard-controller-key")
            .title("The onboarding controller's public key")
            .summary("The SPKI PEM the host verifies entitlements against; bound in no door")
            .verb(Verb::Source)
            .output("application/x-pem-file")
    }
}

/// The lobbies' rate: every `say` on a lobby in the last hour, by the kernel clock.
#[derive(Default)]
pub(crate) struct LobbyRate {
    says: Mutex<BTreeMap<String, VecDeque<(u64, String)>>>,
}

impl LobbyRate {
    /// Count one `say` by `who` (a fingerprint, or the principal when its name carries none)
    /// on `layer` at `now`, or refuse it, naming the bound it would break. A refused say is
    /// not counted.
    pub(crate) fn admit(&self, layer: &str, who: &str, now: u64) -> CoreResult<()> {
        let mut says = self.says.lock().expect("lobby rate lock");
        let window = says.entry(layer.to_string()).or_default();
        while window
            .front()
            .is_some_and(|(at, _)| now.saturating_sub(*at) >= LOBBY_WINDOW_MILLIS)
        {
            window.pop_front();
        }
        if window.len() >= LOBBY_SAYS_TOTAL {
            return Err(Error::Unavailable(format!(
                "lobby `{layer}` has taken its {LOBBY_SAYS_TOTAL} says this hour from everyone \
                 together; try again later"
            )));
        }
        if window.iter().filter(|(_, w)| w == who).count() >= LOBBY_SAYS_PER_FINGERPRINT {
            return Err(Error::Unavailable(format!(
                "lobby `{layer}` takes at most {LOBBY_SAYS_PER_FINGERPRINT} says an hour from one \
                 certificate, and {who} has made them; try again later"
            )));
        }
        window.push_back((now, who.to_string()));
        Ok(())
    }
}
