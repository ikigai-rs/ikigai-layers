//! The public join (ledger #3290): a certificate nobody enrolled asks for `read` and `feed` on a
//! host's public layers, and the host answers by policy, with no person deciding.
//!
//! A `lobby = true` channel with `join = "public"` takes one [`crate::onboard::JOIN_REQUEST`]. The host checks
//! that the poster holds the certificate (as an enrollment lobby does), that every layer named
//! says `public = true`, and the limits below, then writes an [`ENTITLEMENT`] signed under its
//! own key and answers the request with it: the joiner cannot read the lobby, so a reply entry
//! there would never reach it. The grant is live on the joiner's next connection.
//!
//! **What it holds.** `read` and `feed` on exactly the layers it asked for, under the principal
//! `urn:iki:host:{door}:client:{fingerprint}`, never a participant's name: nothing goes into
//! `participants.ttl`, so a write anywhere is refused and no collaboration names it.
//!
//! **Where it lives.** In `<config>/joined/`, which only the host writes, beside the operator's
//! files and never inside them, so an apply from git leaves joiners alone and a joiner never
//! edits an operator file. `<config>/joined/` holds, per identity, `identities/{fp}.json` (the
//! record), `identities/{fp}.ttl` (the signed entitlement) and `clients/{fp}.crt` (the trust
//! set), and the door's own `grants.json` and `clients.json` derived from them. A door that
//! mints through [`crate::Host::door_capability`] reads the same records.
//!
//! **The guards.** Per certificate, per source address where the door says one, a global cap
//! per hour (all three persisted, sealed at rest), a cap on live identities, the deny list
//! [`DENY_FILE`], and expiry: an identity lapses after `join_idle_days` without a read, its
//! certificate leaves the trust set, and it may join again. A restart forgets none of the
//! counts but the per-address one, which begins again under a fresh key. A request is counted
//! by the address its connection came from, so a request on a connection the host no longer
//! remembers is refused, and the stranger reconnects.
//!
//! **No address is kept** (ledger #3307). A source address is counted by its HMAC-SHA256 under
//! a random key held only in memory and replaced every hour, so a hash is comparable only with
//! others of its own hour, and once the key is gone nothing links it to an address. What is
//! hashed is the IPv4 address or the IPv6 network (ledger #3296).
//!
//! **A read is kept to the day** (ledger #3308). An identity's record says which UTC day it last
//! read, written at most once a day, which is all its expiry needs.
//!
//! **Only the operator counts, and only in aggregate** (ledger #3309). The count is three
//! numbers taken from the records, each reported in words when it is small, and only the
//! generated grants of the participants the operator names in `join.count_readers` can read
//! them (ledger #3670, [`crate::config::load_count_readers`]); a joined identity never can: the
//! feeds carry no reader counts.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hmac::{Hmac, Mac};
use ikigai_core::{
    ArgRef, Capability, EndpointSpace, Error, Exact, Fallback, Iri, Kernel, Request,
    Result as CoreResult, Space, Time, Verb,
};
use ikigai_fn::aggregate::Count;
use oxrdf::{Literal, Triple};
use serde::{Deserialize, Serialize};

use super::apply::KeyEndpoint;
use super::{
    fingerprint_of_pem, join_subject_fingerprint, one_literal, signing_bytes, CERTIFICATE,
    ENTITLEMENT, EXPIRES, FINGERPRINT, LAYER, LAYER_GRANT, PRINCIPAL, REQUEST, SIGNATURE,
};
use crate::at_rest::{self, Sealer};
use crate::channel::invalid;
use crate::config::{Config, JoinConfig};
use crate::grants::{Client, Enrollment};
use crate::host::Host;
use crate::name::{
    fold_fingerprint, is_layer_name, layer_cap, layer_iri, Participant, HOST_CLIENT_PREFIX,
};
use crate::rdf::{self, node, object_iri};
use crate::time::xsd_date_time;

/// The deny list, in the config home: one sha256 per line, `#` starting a comment. Honored at
/// join and on every connection.
pub const DENY_FILE: &str = "deny.txt";
/// The directory joined identities live in, in the config home; only the host writes it.
pub const JOINED_DIR: &str = "joined";
/// The persisted rates, sealed at rest, in the state directory (no layer is named with a dot).
pub const LIMITS_FILE: &str = ".join-limits.age";

pub(crate) const IDENTITIES: &str = "identities";
const TRUST: &str = "clients";
/// Where the host's key is served in its signing kernel, which no door binds.
const HOST_KEY: &str = "urn:layer:onboard:join-host-key";
const SIGN: &str = "urn:sign:sign";
const WINDOW_MILLIS: u64 = 60 * 60 * 1000;
const DAY_MILLIS: u64 = 24 * WINDOW_MILLIS;
/// The most stranger connections the host remembers: one who connects and never joins cannot
/// grow this without bound. When it is full the oldest is forgotten, and a request on a
/// connection the host no longer remembers is refused (see [`Join::admit`]), so forgetting
/// one never lets a request go uncounted by its address.
const SOURCES_MAX: usize = 4096;
/// The most connections the host remembers from one source address. A connection past it is
/// not remembered, and its request is refused, so many connections from one address cannot
/// push out its own earlier ones, nor fill [`SOURCES_MAX`] for everyone else.
const SOURCES_PER_ADDRESS: usize = 256;

/// One joined identity's record.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Identity {
    fingerprint: String,
    principal: String,
    entitlement: String,
    /// The layers it may read, sorted.
    layers: Vec<String>,
    joined_at: u64,
    /// The UTC midnight that began the day of the last read (see [`day_of`]), or of the join
    /// before any read. A file written before ledger #3308 holds a later instant of that day,
    /// and is rounded and rewritten at open.
    last_read: u64,
    certificate: String,
}

impl Identity {
    fn scopes(&self) -> Vec<String> {
        let mut caps: Vec<String> = self
            .layers
            .iter()
            .flat_map(|l| [layer_cap("read", l), layer_cap("feed", l)])
            .collect();
        caps.sort();
        caps
    }

    /// Counted from the day of the last read, or from the join itself when no later day has
    /// been read, so that an identity never read lapses exactly when its `onb:expires` says.
    fn lapsed(&self, now: u64, idle_days: u64) -> bool {
        now.saturating_sub(self.last_read.max(self.joined_at))
            >= idle_days.saturating_mul(DAY_MILLIS)
    }
}

/// The UTC midnight that began the day of `millis`. A day is all the idle expiry needs, so it
/// is all a record keeps of a read (ledger #3308), and a reader's every read does not write a
/// file.
fn day_of(millis: u64) -> u64 {
    millis - millis % DAY_MILLIS
}

/// One counted attempt at the join lobby.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Attempt {
    at: u64,
    fingerprint: String,
    /// The keyed hash of its source address, comparable only under the key of its hour. A
    /// file written before ledger #3307 says `address` here, which is read as no hash at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

/// The key a source address is hashed under: random, in memory only, and never older than
/// [`WINDOW_MILLIS`].
struct SourceKey {
    bytes: [u8; 32],
    since: u64,
}

/// What the host remembers of a stranger's latest connection, for the request it may make on it.
struct Connection {
    /// The keyed hash of the source address the door gave, under the key held now; `None`
    /// when the door gave no address.
    source: Option<String>,
    /// Whether `source` was carried over from the previous key. A connection is remembered
    /// across one key's retirement and forgotten at the next, so the hashes that can be
    /// counted come from two keys at most.
    carried: bool,
    /// The order it was remembered in: the oldest is forgotten first when the host is full.
    order: u64,
}

#[derive(Default)]
struct State {
    identities: BTreeMap<String, Identity>,
    attempts: Vec<Attempt>,
    /// Each stranger's latest connection, by fingerprint.
    connections: HashMap<String, Connection>,
    /// The `order` the next remembered connection takes.
    next_order: u64,
    /// This hour's key, drawn when the first address of the hour is hashed.
    key: Option<SourceKey>,
}

impl State {
    /// Retire a key whose hour is up. Every hash made under it is gone: an attempt's matches
    /// nothing now, and a remembered connection's is carried to a fresh key, as the keyed hash
    /// of the old one, so connections from one address still count together; one carried
    /// already is forgotten. A fresh key that cannot be drawn forgets every address.
    fn retire_key(&mut self, now: u64) {
        if self
            .key
            .as_ref()
            .is_none_or(|key| now.saturating_sub(key.since) < WINDOW_MILLIS)
        {
            return;
        }
        self.key = None;
        for attempt in &mut self.attempts {
            attempt.source = None;
        }
        self.connections
            .retain(|_, c| !(c.carried && c.source.is_some()));
        if !self.connections.values().any(|c| c.source.is_some()) {
            return;
        }
        if let Err(e) = self.draw_key(now) {
            eprintln!("ikigai-layers: join: {e}; connections waiting to join must reconnect");
            self.connections.retain(|_, c| c.source.is_none());
            return;
        }
        let key = self.key.take().expect("drawn above");
        for connection in self.connections.values_mut() {
            if let Some(old) = connection.source.take() {
                connection.source = Some(mac(&key, &format!("carried {old}")));
                connection.carried = true;
            }
        }
        self.key = Some(key);
    }

    fn draw_key(&mut self, now: u64) -> Result<(), String> {
        if self.key.is_none() {
            let mut bytes = [0; 32];
            getrandom::getrandom(&mut bytes)
                .map_err(|e| format!("the hourly key for source addresses cannot be drawn: {e}"))?;
            self.key = Some(SourceKey { bytes, since: now });
        }
        Ok(())
    }

    /// `address` as this hour's HMAC-SHA256, lowercase hex, drawing the hour's key if none is
    /// held. Call [`State::retire_key`] first.
    fn hash_source(&mut self, address: &str, now: u64) -> Result<String, String> {
        self.draw_key(now)?;
        Ok(mac(self.key.as_ref().expect("drawn above"), address))
    }

    /// Remember `fingerprint`'s connection from `source`, replacing its last one, unless its
    /// address already has [`SOURCES_PER_ADDRESS`] others remembered; then it is not, and
    /// those keep theirs. A full host forgets its oldest connection to make room.
    fn remember(&mut self, fingerprint: &str, source: Option<String>) {
        if let Some(hash) = &source {
            let same = self
                .connections
                .iter()
                .filter(|(f, c)| f.as_str() != fingerprint && c.source.as_ref() == Some(hash))
                .count();
            if same >= SOURCES_PER_ADDRESS {
                self.connections.remove(fingerprint);
                return;
            }
        }
        if !self.connections.contains_key(fingerprint) && self.connections.len() >= SOURCES_MAX {
            let oldest = self
                .connections
                .iter()
                .min_by_key(|(_, c)| c.order)
                .map(|(f, _)| f.clone());
            if let Some(oldest) = oldest {
                self.connections.remove(&oldest);
            }
        }
        let order = self.next_order;
        self.next_order += 1;
        self.connections.insert(
            fingerprint.to_string(),
            Connection {
                source,
                carried: false,
                order,
            },
        );
    }
}

/// `input` under `key` as HMAC-SHA256, lowercase hex.
fn mac(key: &SourceKey, input: &str) -> String {
    let mut mac =
        Hmac::<sha2::Sha256>::new_from_slice(&key.bytes).expect("HMAC takes any key length");
    mac.update(input.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the per-address limit counts for the `source` a door gave (ledger #3296): an IPv4
/// address, or an IPv6 address's first `ipv6_prefix` bits, since one host commonly holds a whole
/// /64. A port is dropped, and an IPv4-mapped IPv6 address is its IPv4 address, or every IPv4
/// client of a dual-stack door would share one network. A source that is not an IP address is
/// counted as given. `ipv6_prefix` is 1 to 128, as [`crate::config::load_join`] checks.
fn counted_source(source: &str, ipv6_prefix: u8) -> String {
    let ip = source
        .parse::<SocketAddr>()
        .map(|socket| socket.ip())
        .or_else(|_| source.parse::<IpAddr>());
    match ip.map(|ip| ip.to_canonical()) {
        Ok(IpAddr::V4(ip)) => ip.to_string(),
        Ok(IpAddr::V6(ip)) => {
            let mask = u128::MAX << (128 - u32::from(ipv6_prefix));
            Ipv6Addr::from_bits(ip.to_bits() & mask).to_string()
        }
        Err(_) => source.to_string(),
    }
}

/// The host's public join: its configuration, its own signing kernel, and who has joined.
pub(crate) struct Join {
    pub(crate) config: JoinConfig,
    /// The layers that say `public = true` in the configuration this host opened with.
    public: BTreeSet<String>,
    dir: PathBuf,
    config_dir: PathBuf,
    limits_path: PathBuf,
    sealer: Sealer,
    signer: Kernel,
    state: Mutex<State>,
}

/// A join the host granted: what the request's own answer says.
pub(crate) struct Granted {
    request: String,
    entitlement: String,
    principal: String,
    expires: String,
    layers: Vec<String>,
    granted: Vec<String>,
}

impl Granted {
    /// The answer as plain text: one labeled line per fact.
    pub(crate) fn text(&self) -> String {
        let mut out = format!(
            "entitlement {}\nprincipal {}\nexpires {}\n",
            self.entitlement, self.principal, self.expires
        );
        for cap in &self.granted {
            out.push_str(&format!("granted {cap}\n"));
        }
        out
    }

    /// The answer as JSON, the contract's `granted` among its members.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "@id": self.request,
            "granted": self.granted,
            "entitlement": self.entitlement,
            "principal": self.principal,
            "expires": self.expires,
            "layers": self.layers.iter().map(|l| layer_iri(l)).collect::<Vec<_>>(),
        })
    }
}

/// How many joined identities there are and how many read lately. Upper bounds on people: one
/// person may hold several certificates.
pub(crate) struct Counts {
    joined: Count,
    read_in_7_days: Count,
    read_in_30_days: Count,
}

impl Counts {
    /// The answer as plain text: one labeled line per count.
    pub(crate) fn text(&self) -> String {
        format!(
            "joined {}\nread-in-7-days {}\nread-in-30-days {}\n",
            self.joined, self.read_in_7_days, self.read_in_30_days
        )
    }

    /// The answer as JSON: each member a number, or words under the threshold.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "joined": self.joined,
            "readIn7Days": self.read_in_7_days,
            "readIn30Days": self.read_in_30_days,
        })
    }
}

/// The deny list under `config_dir`, as a set, and the first line that does not read.
///
/// A line that is not a fingerprint is never read as an entry and never dropped silently: the
/// caller decides, and a join refuses over it.
pub(crate) fn read_deny(config_dir: &Path) -> (BTreeSet<String>, Option<String>) {
    let path = config_dir.join(DENY_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (BTreeSet::new(), None),
        Err(e) => {
            return (
                BTreeSet::new(),
                Some(format!("read {}: {e}", path.display())),
            )
        }
    };
    let mut denied = BTreeSet::new();
    let mut problem = None;
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        match fold_fingerprint(line) {
            Some(fp) => {
                denied.insert(fp);
            }
            None => {
                problem.get_or_insert_with(|| {
                    format!(
                        "{} line {}: `{line}` is not a SHA-256 fingerprint (64 hex digits)",
                        path.display(),
                        n + 1
                    )
                });
            }
        }
    }
    (denied, problem)
}

/// The sentence a denied certificate is refused with.
pub(crate) fn denied(fingerprint: &str) -> Error {
    Error::Denied(
        format!(
            "certificate {fingerprint} is on this host's deny list, so it may not join or connect"
        )
        .into(),
    )
}

/// Check a request beyond its shapes: the poster holds the certificate it states (the name the
/// door minted carries the fingerprint, and the posted PEM hashes to it), and every layer named
/// is public. Answers the layers, sorted. Refused whole: a layer that is not public, or does
/// not exist, grants nothing at all, and the two are worded alike so that a refusal never says
/// which private layers exist.
pub(crate) fn check_request(
    host: &Host,
    triples: &[Triple],
    posted: Option<&str>,
    author: &str,
) -> CoreResult<Vec<String>> {
    let poster = Participant::parse(author)
        .ok()
        .and_then(|p| p.fingerprint().map(str::to_string))
        .ok_or_else(|| {
            Error::Denied(
                format!(
                    "a join request is said by the certificate it joins, named by its fingerprint \
                 ({HOST_CLIENT_PREFIX}{{host}}:client:<fingerprint>); {author} carries none"
                )
                .into(),
            )
        })?;
    let stated = one_literal(triples, FINGERPRINT)
        .ok_or_else(|| invalid("a join request states exactly one onb:fingerprint"))?;
    if let Some(named) = posted.and_then(join_subject_fingerprint) {
        if named != stated {
            return Err(invalid(format!(
                "the request is named for {named} and states onb:fingerprint {stated}"
            )));
        }
    }
    if stated != poster {
        return Err(Error::Denied(
            format!(
                "onb:fingerprint {stated} is not the certificate that said it ({poster}): a \
             certificate asks to join for itself only"
            )
            .into(),
        ));
    }
    let pem = one_literal(triples, CERTIFICATE)
        .ok_or_else(|| invalid("a join request carries exactly one onb:certificate"))?;
    let actual = fingerprint_of_pem(pem).map_err(|e| invalid(format!("onb:certificate: {e}")))?;
    if actual != stated {
        return Err(invalid(format!(
            "onb:certificate hashes to {actual}, not to its onb:fingerprint {stated}"
        )));
    }
    let mut names = BTreeSet::new();
    for t in triples.iter().filter(|t| t.predicate.as_str() == LAYER) {
        let iri =
            object_iri(&t.object).ok_or_else(|| invalid("an onb:layer that is not an IRI"))?;
        let name = iri
            .strip_prefix("urn:layer:")
            .filter(|n| is_layer_name(n))
            .ok_or_else(|| invalid(format!("onb:layer {iri} is not urn:layer:{{name}}")))?;
        names.insert(name.to_string());
    }
    if names.is_empty() {
        return Err(invalid(
            "a join request names one or more layers, each with onb:layer",
        ));
    }
    for name in &names {
        let public = host
            .layer(name)
            .and_then(|s| s.def.feed.as_ref())
            .is_some_and(|f| f.public);
        if !public {
            return Err(Error::Denied(
                format!(
                "layer `{name}` is not a public layer of this host: a join grants read and feed \
                 on public layers only, so nothing in this request was granted"
            )
                .into(),
            ));
        }
    }
    Ok(names.into_iter().collect())
}

impl Join {
    /// Read what the host already holds: the joined identities and the rates, the host's own
    /// state, so that neither a restart nor an operator's apply forgets one. Identities past
    /// their idle limit lapse here, and a certificate no record names leaves the trust set. The
    /// hashes of source addresses are not read: their key went with the last process.
    ///
    /// `public` is the layers that say `public = true` now. An identity holding a layer that no
    /// longer does (made private, or gone) is revoked here whole, so making a layer private
    /// takes it from every joiner at the next open, and the joiner may join again for what is
    /// still public.
    pub(crate) fn load(
        config: JoinConfig,
        public: BTreeSet<String>,
        config_dir: &Path,
        state_dir: &Path,
        sealer: &Sealer,
        reseal: bool,
        now: Time,
    ) -> Result<Join, String> {
        let dir = config_dir.join(JOINED_DIR);
        for sub in [dir.clone(), dir.join(IDENTITIES), dir.join(TRUST)] {
            at_rest::private_dir(&sub).map_err(|e| format!("create {}: {e}", sub.display()))?;
        }
        let key = EndpointSpace::new().bind(
            Exact::new(HOST_KEY),
            KeyEndpoint(config.host_key.clone().into_bytes()),
        );
        let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_sign::space()), Arc::new(key)];
        let join = Join {
            config,
            public,
            dir,
            config_dir: config_dir.to_path_buf(),
            limits_path: state_dir.join(LIMITS_FILE),
            sealer: sealer.clone(),
            signer: Kernel::new(Arc::new(Fallback::new(spaces))),
            state: Mutex::new(State::default()),
        };
        {
            let mut state = join.state.lock().expect("join lock");
            state.identities = join.read_identities()?;
            state.attempts = join.read_attempts(reseal)?;
            join.reconcile(&mut state, now.as_millis())?;
        }
        Ok(join)
    }

    fn read_identities(&self) -> Result<BTreeMap<String, Identity>, String> {
        let dir = self.dir.join(IDENTITIES);
        let mut identities = BTreeMap::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
            let path = entry
                .map_err(|e| format!("read {}: {e}", dir.display()))?
                .path();
            if path.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let mut identity: Identity =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned());
            if stem.as_deref() != Some(identity.fingerprint.as_str())
                || Participant::parse(&identity.principal)
                    .ok()
                    .and_then(|p| p.fingerprint().map(str::to_string))
                    .as_deref()
                    != Some(identity.fingerprint.as_str())
            {
                return Err(format!(
                    "{} is not the record of certificate {}",
                    path.display(),
                    identity.fingerprint
                ));
            }
            // Rewritten here, so a file from before ledger #3308 gives up its hour at the
            // first open and not at the next read, which may never come.
            let day = day_of(identity.last_read);
            if day != identity.last_read {
                identity.last_read = day;
                let record = serde_json::to_string(&identity).expect("a plain record");
                at_rest::replace(&path, record.as_bytes())?;
            }
            identities.insert(identity.fingerprint.clone(), identity);
        }
        Ok(identities)
    }

    fn read_attempts(&self, reseal: bool) -> Result<Vec<Attempt>, String> {
        if !self.limits_path.exists() {
            return Ok(Vec::new());
        }
        let text = self.sealer.read_text(&self.limits_path)?;
        let mut attempts: Vec<Attempt> = serde_json::from_str(&text)
            .map_err(|e| format!("{}: {e}", self.limits_path.display()))?;
        for attempt in &mut attempts {
            attempt.source = None;
        }
        let bytes = std::fs::read(&self.limits_path)
            .map_err(|e| format!("read {}: {e}", self.limits_path.display()))?;
        // Rewritten whenever this drops something, so a file from before ledger #3307 gives up
        // its addresses at the first open and not at the next attempt, which may never come.
        let dropped = serde_json::to_string(&attempts).expect("a list of plain records") != text;
        if dropped || (reseal && !self.sealer.is_current(&bytes)) {
            self.write_attempts(&attempts)?;
        }
        Ok(attempts)
    }

    fn write_attempts(&self, attempts: &[Attempt]) -> Result<(), String> {
        let text = serde_json::to_string(attempts).expect("a list of plain records");
        at_rest::replace(&self.limits_path, &self.sealer.seal(text.as_bytes())?)
    }

    fn certificate_path(&self, fingerprint: &str) -> PathBuf {
        self.dir.join(TRUST).join(format!("{fingerprint}.crt"))
    }

    fn record_path(&self, fingerprint: &str, suffix: &str) -> PathBuf {
        self.dir
            .join(IDENTITIES)
            .join(format!("{fingerprint}.{suffix}"))
    }

    /// Lapse every identity past its idle limit, revoke every one holding a layer that is not
    /// public now, and make the trust set the live records exactly.
    fn reconcile(&self, state: &mut State, now: u64) -> Result<(), String> {
        let idle = self.config.idle_days;
        let lapsed: Vec<String> = state
            .identities
            .values()
            .filter(|i| i.lapsed(now, idle) || !i.layers.iter().all(|l| self.public.contains(l)))
            .map(|i| i.fingerprint.clone())
            .collect();
        for fp in &lapsed {
            self.remove(state, fp)?;
        }
        let dir = self.dir.join(TRUST);
        for entry in std::fs::read_dir(&dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
            let path = entry
                .map_err(|e| format!("read {}: {e}", dir.display()))?
                .path();
            let live = path
                .file_stem()
                .is_some_and(|s| state.identities.contains_key(s.to_string_lossy().as_ref()));
            if !live {
                std::fs::remove_file(&path)
                    .map_err(|e| format!("remove {}: {e}", path.display()))?;
            }
        }
        for identity in state.identities.values() {
            let path = self.certificate_path(&identity.fingerprint);
            if !path.exists() {
                at_rest::replace(&path, identity.certificate.as_bytes())?;
            }
        }
        self.write_enrollment(state)
    }

    fn write_enrollment(&self, state: &State) -> Result<(), String> {
        let mut enrollment = Enrollment::default();
        for identity in state.identities.values() {
            let participant = Participant::parse(&identity.principal)?;
            enrollment
                .grants
                .insert(participant.slug().to_string(), identity.scopes());
            enrollment.clients.insert(
                identity.fingerprint.clone(),
                Client {
                    grant: participant.slug().to_string(),
                    principal: identity.principal.clone(),
                    label: format!("joined {}", &identity.fingerprint[..12]),
                },
            );
        }
        enrollment.write(&self.dir)
    }

    /// Take one identity out: its certificate leaves the trust set first, so a record is never
    /// the only thing left trusting a removed certificate.
    fn remove(&self, state: &mut State, fingerprint: &str) -> Result<(), String> {
        let files = [
            self.certificate_path(fingerprint),
            self.record_path(fingerprint, "ttl"),
            self.record_path(fingerprint, "json"),
        ];
        for path in &files {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("remove {}: {e}", path.display())),
            }
        }
        state.identities.remove(fingerprint);
        Ok(())
    }

    fn lapse_one(&self, state: &mut State, fingerprint: &str) -> Result<(), String> {
        self.remove(state, fingerprint)?;
        self.write_enrollment(state)
    }

    /// What a door mints for `fingerprint` at a connection: `Ok(Some)` for a live identity; or
    /// `Ok(None)` to fall through to the lobby, remembering the connection, and its source's
    /// hash, for its request. A connection without a source never replaces one with a source:
    /// the stricter count holds.
    pub(crate) fn connect(
        &self,
        fingerprint: &str,
        source: Option<&str>,
        now: Time,
    ) -> Result<Option<Capability>, String> {
        let mut state = self.state.lock().expect("join lock");
        if let Some(identity) = state.identities.get(fingerprint).cloned() {
            if !identity.lapsed(now.as_millis(), self.config.idle_days) {
                return Capability::scoped(identity.scopes())
                    .with_principal(&identity.principal)
                    .map(Some)
                    .map_err(|e| format!("{}: {e}", identity.principal));
            }
            self.lapse_one(&mut state, fingerprint)?;
        }
        state.retire_key(now.as_millis());
        match source {
            Some(address) => {
                let counted = counted_source(address, self.config.ipv6_prefix);
                let hash = state.hash_source(&counted, now.as_millis())?;
                state.remember(fingerprint, Some(hash));
            }
            None => {
                let addressed = state
                    .connections
                    .get(fingerprint)
                    .is_some_and(|c| c.source.is_some());
                if !addressed {
                    state.remember(fingerprint, None);
                }
            }
        }
        Ok(None)
    }

    /// The check on every request a joined identity makes: refuse a lapsed one and record the
    /// read of a live one. Any other principal passes.
    pub(crate) fn gate(&self, fingerprint: &str, principal: &str, now: Time) -> CoreResult<()> {
        let mut state = self.state.lock().expect("join lock");
        let Some(identity) = state
            .identities
            .get_mut(fingerprint)
            .filter(|i| i.principal == principal)
        else {
            return Ok(());
        };
        if identity.lapsed(now.as_millis(), self.config.idle_days) {
            let idle = self.config.idle_days;
            self.lapse_one(&mut state, fingerprint)
                .map_err(|e| Error::Endpoint(format!("lapsing certificate {fingerprint}: {e}")))?;
            return Err(Error::Denied(
                format!(
                "certificate {fingerprint} read nothing here for {idle} days, so it has lapsed \
                 and holds nothing now: it may join again"
            )
                .into(),
            ));
        }
        let today = day_of(now.as_millis());
        if today > identity.last_read {
            identity.last_read = today;
            let record = serde_json::to_string(identity).expect("a plain record");
            let path = self.record_path(fingerprint, "json");
            if let Err(e) = at_rest::replace(&path, record.as_bytes()) {
                eprintln!("ikigai-layers: join: recording a read failed: {e}");
            }
        }
        Ok(())
    }

    /// The live identities at `now`, and how many of them read in the last 7 and 30 UTC days,
    /// today among them. A join counts as a read on its day, since a record holds one day
    /// for both. Read-only: an identity past its limit is left out here and lapses as ever.
    pub(crate) fn counts(&self, now: Time) -> Counts {
        let state = self.state.lock().expect("join lock");
        let now = now.as_millis();
        let today = day_of(now);
        let live: Vec<&Identity> = state
            .identities
            .values()
            .filter(|i| !i.lapsed(now, self.config.idle_days))
            .collect();
        let read_within = |days: u64| {
            live.iter()
                .filter(|i| today.saturating_sub(i.last_read) < days * DAY_MILLIS)
                .count()
        };
        Counts {
            joined: Count::new(live.len()),
            read_in_7_days: Count::new(read_within(7)),
            read_in_30_days: Count::new(read_within(30)),
        }
    }

    /// Count one attempt by `fingerprint` at `now`, or refuse it naming the limit it would
    /// break: its certificate's, its address's where the door said one, or everyone's. A
    /// refused attempt is not counted, and a counted one is on disk before anything else
    /// happens, so a restart cannot reopen a spent window. An attempt on a connection the
    /// host does not remember is refused too, since whether its address has made its
    /// requests cannot be known: the connection was pushed out, came from an address with
    /// too many waiting, or is from before the last key but one.
    pub(crate) fn admit(&self, lobby: &str, fingerprint: &str, now: Time) -> CoreResult<()> {
        let unavailable = |what: String| Error::Unavailable(format!("{what}; try again later"));
        let mut state = self.state.lock().expect("join lock");
        let now = now.as_millis();
        state
            .attempts
            .retain(|a| now.saturating_sub(a.at) < WINDOW_MILLIS);
        state.retire_key(now);
        let connection = state.connections.get(fingerprint).map(|c| c.source.clone());
        let c = &self.config;
        if state.attempts.len() >= c.per_hour {
            return Err(unavailable(format!(
                "join lobby `{lobby}` has taken its {} requests this hour from everyone together",
                c.per_hour
            )));
        }
        if state
            .attempts
            .iter()
            .filter(|a| a.fingerprint == fingerprint)
            .count()
            >= c.per_certificate
        {
            return Err(unavailable(format!(
                "join lobby `{lobby}` takes at most {} requests an hour from one certificate, \
                 and {fingerprint} has made them",
                c.per_certificate
            )));
        }
        let Some(source) = connection else {
            return Err(Error::Unavailable(format!(
                "join lobby `{lobby}` no longer remembers the connection certificate \
                 {fingerprint} asked on, so it cannot count the request by its source address; \
                 reconnect and ask again"
            )));
        };
        if let Some(source) = &source {
            if state
                .attempts
                .iter()
                .filter(|a| a.source.as_ref() == Some(source))
                .count()
                >= c.per_address
            {
                return Err(unavailable(format!(
                    "join lobby `{lobby}` takes at most {} requests an hour from one source \
                     address, counting an IPv6 /{} network as one address, and this one has made \
                     them",
                    c.per_address, c.ipv6_prefix
                )));
            }
        }
        state.attempts.push(Attempt {
            at: now,
            fingerprint: fingerprint.to_string(),
            source,
        });
        self.write_attempts(&state.attempts)
            .map_err(|e| Error::Unavailable(format!("the join rate cannot be recorded: {e}")))
    }

    /// Refuse a certificate that has already joined, or a host already at its cap on live
    /// identities. A joined identity holds read and feed only, so the lobby is not for it.
    pub(crate) fn check_room(&self, fingerprint: &str, now: Time) -> CoreResult<()> {
        let mut state = self.state.lock().expect("join lock");
        self.reconcile(&mut state, now.as_millis())
            .map_err(Error::Endpoint)?;
        if state.identities.contains_key(fingerprint) {
            return Err(Error::Denied(
                format!(
                "certificate {fingerprint} has already joined this host, and a joined identity \
                 only reads"
            )
                .into(),
            ));
        }
        if state.identities.len() >= self.config.max_joined {
            return Err(Error::Unavailable(format!(
                "this host holds {} joined identities, the most its `join.max_joined` allows; \
                 try again later",
                self.config.max_joined
            )));
        }
        Ok(())
    }

    async fn sign(&self, bytes: Vec<u8>) -> Result<String, String> {
        let request = Request::new(Verb::Source, Iri::parse(SIGN).expect("a constant IRI"))
            .with_arg("in", ArgRef::Inline(bytes))
            .with_arg("key", ArgRef::Inline(HOST_KEY.as_bytes().to_vec()));
        let graph = self
            .signer
            .issue(request, &Capability::scoped(["urn:cap:sign"]))
            .await
            .map_err(|e| e.to_string())?;
        String::from_utf8(graph.bytes).map_err(|_| "the signature is not UTF-8".to_string())
    }

    /// Sign a probe, at open: a host key that cannot sign is the operator's error, stopped
    /// before a stranger meets it.
    pub(crate) async fn probe(&self) -> Result<(), String> {
        self.sign(b"ikigai-layers: the join key signs".to_vec())
            .await
            .map(|_| ())
    }

    /// Sign the entitlement for a request (entry `request`, said by `principal`) and record the
    /// identity. The file carrying the record is written before the certificate joins the
    /// trust set, so a stop between the two leaves a record the next open repairs and never a
    /// trusted certificate no record names.
    pub(crate) async fn grant(
        &self,
        request: &str,
        principal: &str,
        certificate: &str,
        layers: &[String],
        id: &str,
        now: Time,
    ) -> Result<Granted, String> {
        let participant = Participant::parse(principal)?;
        let fingerprint = participant
            .fingerprint()
            .ok_or_else(|| format!("{principal} carries no fingerprint"))?
            .to_string();
        let door = principal
            .strip_prefix(HOST_CLIENT_PREFIX)
            .and_then(|rest| rest.split_once(":client:"))
            .map(|(door, _)| door)
            .ok_or_else(|| format!("{principal} names no door"))?;
        let iri = format!("{HOST_CLIENT_PREFIX}{door}:entitlement:{id}");
        let scopes: Vec<String> = layers
            .iter()
            .flat_map(|l| [layer_cap("read", l), layer_cap("feed", l)])
            .collect();
        let expires = xsd_date_time(Time::from_millis(
            now.as_millis()
                .saturating_add(self.config.idle_days.saturating_mul(DAY_MILLIS)),
        ));
        let subject = node(&iri);
        let mut triples = vec![
            Triple::new(subject.clone(), node(rdf::RDF_TYPE), node(ENTITLEMENT)),
            Triple::new(
                subject.clone(),
                node(FINGERPRINT),
                Literal::new_simple_literal(fingerprint.as_str()),
            ),
            Triple::new(subject.clone(), node(PRINCIPAL), node(principal)),
            Triple::new(subject.clone(), node(REQUEST), node(request)),
            Triple::new(
                subject.clone(),
                node(EXPIRES),
                Literal::new_typed_literal(expires.as_str(), node(rdf::XSD_DATE_TIME)),
            ),
        ];
        for cap in &scopes {
            triples.push(Triple::new(
                subject.clone(),
                node(LAYER_GRANT),
                node(cap.as_str()),
            ));
        }
        let signature = self.sign(signing_bytes(&triples, &iri)).await?;
        triples.push(Triple::new(
            subject,
            node(SIGNATURE),
            Literal::new_simple_literal(signature),
        ));
        let identity = Identity {
            fingerprint: fingerprint.clone(),
            principal: principal.to_string(),
            entitlement: iri.clone(),
            layers: layers.to_vec(),
            joined_at: now.as_millis(),
            last_read: day_of(now.as_millis()),
            certificate: certificate.to_string(),
        };
        let mut state = self.state.lock().expect("join lock");
        at_rest::replace(
            &self.record_path(&fingerprint, "ttl"),
            rdf::to_turtle(&triples).as_bytes(),
        )?;
        let record = serde_json::to_string(&identity).expect("a plain record");
        at_rest::replace(&self.record_path(&fingerprint, "json"), record.as_bytes())?;
        at_rest::replace(&self.certificate_path(&fingerprint), certificate.as_bytes())?;
        let mut granted = scopes.clone();
        granted.sort();
        state.identities.insert(fingerprint, identity);
        self.write_enrollment(&state)?;
        Ok(Granted {
            request: request.to_string(),
            entitlement: iri,
            principal: principal.to_string(),
            expires,
            layers: layers.to_vec(),
            granted,
        })
    }

    /// The deny list's refusal at join: a denied certificate, or a list that does not read.
    pub(crate) fn check_deny(&self, fingerprint: &str) -> CoreResult<()> {
        let (list, problem) = read_deny(&self.config_dir);
        if list.contains(fingerprint) {
            return Err(denied(fingerprint));
        }
        if let Some(problem) = problem {
            return Err(Error::Unavailable(format!(
                "this host's deny list does not read ({problem}), so it takes no join until the \
                 operator fixes it"
            )));
        }
        Ok(())
    }
}

/// The JSON a host publishes at `https://<host>/.well-known/ikigai/layers.json`.
///
/// The join lobby and the public layers come from the configuration under `config_dir`, and the
/// certificate's sha256 from `door_dir/server.crt`. `door` is `quic://host:port`.
pub fn descriptor(config_dir: &Path, door: &str, door_dir: &Path) -> Result<Descriptor, String> {
    let address = door
        .strip_prefix("quic://")
        .filter(|rest| rest.contains(':') && !rest.contains('/'));
    if address.is_none() {
        return Err(format!("`{door}` is not a door address (quic://host:port)"));
    }
    let config = Config::load(config_dir).map_err(|e| e.to_string())?;
    let lobby = config
        .layers
        .values()
        .find(|d| d.join)
        .map(|d| d.name.clone())
        .ok_or_else(|| {
            "no layer says `join = \"public\"`: a descriptor names the join lobby".to_string()
        })?;
    let public: Vec<serde_json::Value> = config
        .layers
        .values()
        .filter(|d| d.feed.as_ref().is_some_and(|f| f.public))
        .map(|d| {
            let mut layer = serde_json::json!({
                "name": d.name,
                "type": "ik:Feed",
                "read_only": true,
            });
            if let Some(label) = &d.label {
                layer["label"] = serde_json::Value::String(label.clone());
            }
            layer
        })
        .collect();
    if public.is_empty() {
        return Err(
            "no layer says `public = true`: a descriptor offers at least one public feed".into(),
        );
    }
    let crt = door_dir.join("server.crt");
    let pem = std::fs::read_to_string(&crt).map_err(|e| format!("read {}: {e}", crt.display()))?;
    let sha256 = fingerprint_of_pem(&pem).map_err(|e| format!("{}: {e}", crt.display()))?;
    Ok(Descriptor {
        json: serde_json::json!({
            "door": door,
            "server_cert": { "url": "server.crt", "sha256": sha256 },
            "join": format!("urn:layer:{lobby}:say"),
            "public_layers": public,
        }),
        server_cert: pem,
    })
}

/// What [`descriptor`] derived: the document and the server certificate it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// The document, `layers.json`.
    pub json: serde_json::Value,
    /// The door's `server.crt`, PEM, published beside it.
    pub server_cert: String,
}

impl Descriptor {
    /// Write `layers.json` and `server.crt` into `out`, each atomically. Answers both paths.
    pub fn write(&self, out: &Path) -> Result<(PathBuf, PathBuf), String> {
        std::fs::create_dir_all(out).map_err(|e| format!("create {}: {e}", out.display()))?;
        let mut text = serde_json::to_string_pretty(&self.json).expect("a JSON value");
        text.push('\n');
        let (json, crt) = (out.join("layers.json"), out.join("server.crt"));
        super::apply::write_atomic(&json, &text)?;
        super::apply::write_atomic(&crt, &self.server_cert)?;
        Ok((json, crt))
    }
}
