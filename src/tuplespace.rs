//! The `tuplespace` model: offers and addressed reports, taken atomically, with receipts.
//!
//! A tuplespace layer FRONTS an ikigai-intray space (`urn:space:{name}`'s endpoint, linked
//! and invoked privately under the host's own `urn:cap:space:*` authority, over
//! `<state>/{layer}/tuples/`). Intray gives the space: content-addressed drops and an
//! atomic take (a rename, so of two takers exactly one wins). This module adds what a
//! SHARED task layer needs and intray deliberately does not know, so intray itself is
//! unchanged:
//!
//! - **a name per tuple**: exactly one subject, an IRI under `urn:layer:{layer}:` that the
//!   dropper mints (`…:offer:{ulid}`, `…:report:{ulid}`), never used before in the layer,
//!   pending or taken. The intray id stays the content hash; the subject is the name
//!   everyone else uses;
//! - **stamps**: who dropped it (`prov:wasAttributedTo`), when (`dcterms:created`) and its
//!   place in the feed (`ik:seq`), stamped INTO the tuple, so the content-addressed id
//!   covers them, and refused if sent;
//! - **receipts**: a take writes `<state>/{layer}/receipts/{id}.ttl` (the take as a
//!   `prov:Activity` naming the taker, and the tuple as it was taken) BEFORE it replies,
//!   so a taker that lost its reply finds its own take;
//! - **the addressed take**: in a layer defined `addressed = true`, a tuple carrying
//!   `ik:addressedTo` is taken only by a participant it names;
//! - **reports on offers**: a tuple naming `ik:offer <X>` is admitted only from the
//!   participant whose take of `X` this layer has a receipt for, so nobody hands back work
//!   they never took; in an addressed layer it must be addressed to `X`'s offerer alone, so
//!   it reaches the offerer and nobody else, exactly once.
//!
//! # Crash order
//!
//! A take stages its receipt (`receipts/.taking/{cid}.{id}.ttl`), then takes from intray,
//! then commits the receipt (a rename), then logs the feed event. On open, a staged receipt
//! whose tuple is still pending is a take that never happened (removed); one whose tuple
//! is gone is a take that did (committed). Events stored but not logged are logged then, in
//! order, as the channel model does.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::OpenOptions;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

use ikigai_core::{
    ArgRef, Bindings, Capability, Endpoint, Error, Invocation, Iri, Request, Result, Verb,
};
use ikigai_intray::SpaceEndpoint;
use oxrdf::{Literal, NamedOrBlankNode, Term, Triple};

use crate::channel::{admitted_type, invalid, parse_content, refuse_stamp, require_type};
use crate::config::LayerDef;
use crate::feed::Event;
use crate::id;
use crate::rdf::{self, node, object_iri, subject_iri};

/// The intray space a tuplespace layer fronts: `<state>/{layer}/tuples/`.
pub const SPACE_NAME: &str = "tuples";

/// The words a tuple's subject may not begin with after `urn:layer:{layer}:`: the host's own
/// resources (and `new`, the channel's placeholder), so a tuple's name never reads as one.
pub const RESERVED: [&str; 11] = [
    "new", "shapes", "context", "feed", "tuples", "receipt", "say", "entries", "entry", "graph",
    "latest",
];

/// The longest subject IRI a tuple may carry.
pub const MAX_SUBJECT: usize = 512;

/// What the index knows about one tuple.
#[derive(Clone, Debug)]
pub struct TupleMeta {
    /// The sequence number of its `out` event.
    pub seq: u64,
    /// Its name: the one subject, under `urn:layer:{layer}:`.
    pub subject: String,
    /// Its intray id (the blake3 of its stamped Turtle) while it is pending; empty once
    /// taken, when its bytes have left the space.
    pub cid: String,
    /// Who dropped it.
    pub dropper: String,
    /// When, as `xsd:dateTime`.
    pub created: String,
    /// Its `rdf:type`s.
    pub types: BTreeSet<String>,
    /// The participants it is addressed to (`ik:addressedTo`).
    pub addressed_to: BTreeSet<String>,
    /// The offer it reports on (`ik:offer`), for a report.
    pub offer: Option<String>,
    /// A digest of what the dropper sent (the tuple without its stamps), so a replay of the
    /// same drop is recognized as one.
    digest: u64,
}

/// A take, as its receipt records it.
#[derive(Clone, Debug)]
pub struct Receipt {
    /// The sequence number of its `taken` event.
    pub seq: u64,
    /// Its id: [`id::mint`] over its time and sequence number.
    pub id: String,
    /// `urn:layer:{layer}:receipt:{id}`.
    pub iri: String,
    /// Who took the tuple (the principal its door minted).
    pub taker: String,
    /// When, as `xsd:dateTime`.
    pub time: String,
    /// The tuple it took.
    pub tuple: TupleMeta,
}

/// Where a subject is now.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// Pending, by its `out` sequence number.
    Pending(u64),
    /// Taken, by its index in the receipts.
    Taken(usize),
    /// Logged as dropped, but neither pending nor taken through this host: removed from the
    /// space by something else. Its name stays used.
    Gone,
}

/// A subject's state, as [`Index::lookup`] answers it.
#[derive(Clone, Debug)]
pub enum Lookup<'a> {
    /// Never used in this layer.
    Unknown,
    /// Pending.
    Pending(&'a TupleMeta),
    /// Taken, with its receipt.
    Taken(&'a Receipt),
    /// Removed from the space without a receipt.
    Gone,
}

/// The `urn:layer:{layer}:receipt:` prefix.
pub fn receipt_prefix(layer: &str) -> String {
    format!("urn:layer:{layer}:receipt:")
}

/// A tuplespace layer's index: pending tuples in feed order, receipts in take order, and
/// every subject ever used. Derived on open from the space, the receipts and the feed: they
/// are the record.
#[derive(Default)]
pub struct Index {
    pending: BTreeMap<u64, TupleMeta>,
    by_cid: HashMap<String, u64>,
    receipts: Vec<Receipt>,
    by_receipt_id: HashMap<String, usize>,
    subjects: HashMap<String, Place>,
}

impl Index {
    /// Where `subject` is.
    pub fn lookup(&self, subject: &str) -> Lookup<'_> {
        match self.subjects.get(subject) {
            None => Lookup::Unknown,
            Some(Place::Pending(seq)) => Lookup::Pending(&self.pending[seq]),
            Some(Place::Taken(i)) => Lookup::Taken(&self.receipts[*i]),
            Some(Place::Gone) => Lookup::Gone,
        }
    }

    /// The pending tuples, in the order they were dropped.
    pub fn pending(&self) -> impl Iterator<Item = &TupleMeta> {
        self.pending.values()
    }

    /// A pending tuple, by its intray id.
    pub fn pending_by_cid(&self, cid: &str) -> Option<&TupleMeta> {
        self.by_cid.get(cid).map(|seq| &self.pending[seq])
    }

    /// The receipts, in the order the takes happened.
    pub fn receipts(&self) -> &[Receipt] {
        &self.receipts
    }

    /// One receipt, by id.
    pub fn receipt(&self, id: &str) -> Option<&Receipt> {
        self.by_receipt_id.get(id).map(|i| &self.receipts[*i])
    }

    fn insert_pending(&mut self, meta: TupleMeta) -> std::result::Result<(), String> {
        if self.subjects.contains_key(&meta.subject) {
            return Err(format!("{} is stored twice", meta.subject));
        }
        self.subjects
            .insert(meta.subject.clone(), Place::Pending(meta.seq));
        self.by_cid.insert(meta.cid.clone(), meta.seq);
        self.pending.insert(meta.seq, meta);
        Ok(())
    }

    fn insert_receipt(&mut self, receipt: Receipt) -> std::result::Result<(), String> {
        if self.subjects.contains_key(&receipt.tuple.subject) {
            return Err(format!("{} is stored twice", receipt.tuple.subject));
        }
        let i = self.receipts.len();
        self.subjects
            .insert(receipt.tuple.subject.clone(), Place::Taken(i));
        self.by_receipt_id.insert(receipt.id.clone(), i);
        self.receipts.push(receipt);
        Ok(())
    }

    /// Record a take: the pending tuple becomes a receipt.
    fn taken(&mut self, receipt: Receipt) {
        if let Some(meta) = self.pending.remove(&receipt.tuple.seq) {
            self.by_cid.remove(&meta.cid);
        }
        self.subjects.remove(&receipt.tuple.subject);
        self.insert_receipt(receipt)
            .expect("a subject just removed from the index");
    }

    /// Record that a pending tuple left the space without a receipt.
    fn gone(&mut self, subject: &str) {
        if let Some(Place::Pending(seq)) = self.subjects.get(subject).copied() {
            if let Some(meta) = self.pending.remove(&seq) {
                self.by_cid.remove(&meta.cid);
            }
        }
        self.subjects.insert(subject.to_string(), Place::Gone);
    }
}

/// The intray space a layer fronts, invoked privately under the host's own authority.
///
/// The endpoint is never bound into a kernel: participants hold no `urn:cap:space:*` and
/// no path reaches it but this host's own calls. That is how the host "holds the space
/// caps" without an endpoint ever widening a caller's authority: core gives an endpoint no
/// way to issue a sub-request above its caller's capability, so the space is a value the
/// host owns rather than a name it resolves.
pub struct Store {
    space: SpaceEndpoint,
    receipts: PathBuf,
    staging: PathBuf,
    authority: Capability,
}

fn space_iri() -> Iri {
    Iri::parse(format!("urn:space:{SPACE_NAME}")).expect("a constant IRI")
}

impl Store {
    fn open(dir: &Path) -> std::result::Result<Store, String> {
        let receipts = dir.join("receipts");
        let staging = receipts.join(".taking");
        std::fs::create_dir_all(&staging)
            .map_err(|e| format!("create {}: {e}", staging.display()))?;
        Ok(Store {
            space: SpaceEndpoint::new(dir.to_path_buf()),
            receipts,
            staging,
            authority: Capability::scoped([
                ikigai_intray::CAP_OUT.to_string(),
                ikigai_intray::CAP_READ.to_string(),
                ikigai_intray::CAP_TAKE.to_string(),
            ]),
        })
    }

    async fn call(&self, verb: Verb, args: &[(&str, &[u8])]) -> Result<Vec<u8>> {
        let mut request = Request::new(verb, space_iri());
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.to_vec()));
        }
        let mut bindings = Bindings::new();
        bindings.insert("name", SPACE_NAME);
        let inv = Invocation::detached(&request, &bindings, &self.authority);
        self.space.invoke(&inv).await.map(|r| r.bytes)
    }

    /// Drop `bytes` into the space; answers the intray id.
    pub async fn drop_tuple(&self, bytes: &[u8]) -> Result<String> {
        let id = self.call(Verb::Sink, &[("content", bytes)]).await?;
        String::from_utf8(id).map_err(|_| Error::Endpoint("intray answered a non-UTF-8 id".into()))
    }

    /// A pending tuple's bytes, or `None` when it is not in the space.
    pub async fn read(&self, cid: &str) -> Result<Option<Vec<u8>>> {
        match self.call(Verb::Source, &[("tuple", cid.as_bytes())]).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Take a tuple by its intray id: its bytes, or `None` when another taker won it.
    pub async fn take(&self, cid: &str) -> Result<Option<Vec<u8>>> {
        match self.call(Verb::Delete, &[("tuple", cid.as_bytes())]).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// The intray ids pending in the space.
    pub async fn list(&self) -> Result<Vec<String>> {
        let bytes = self.call(Verb::Source, &[]).await?;
        Ok(ids(&bytes))
    }

    /// The intray ids of the pending tuples a SPARQL ASK holds for (intray's associative
    /// match: bounded, and refusing `SERVICE`).
    pub async fn matching(&self, query: &str) -> Result<Vec<String>> {
        let bytes = self
            .call(Verb::Source, &[("match", query.as_bytes())])
            .await?;
        Ok(ids(&bytes))
    }

    fn receipt_path(&self, id: &str) -> PathBuf {
        self.receipts.join(format!("{id}.ttl"))
    }

    fn staged_path(&self, cid: &str, id: &str) -> PathBuf {
        self.staging.join(format!("{cid}.{id}.ttl"))
    }

    /// Stage a receipt before its take: written once and synced.
    pub fn stage(&self, cid: &str, id: &str, text: &str) -> std::result::Result<(), String> {
        let path = self.staged_path(cid, id);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("create {}: {e}", path.display()))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("write {}: {e}", path.display()))
    }

    /// Drop a staged receipt whose take did not happen.
    pub fn unstage(&self, cid: &str, id: &str) {
        let _ = std::fs::remove_file(self.staged_path(cid, id));
    }

    /// Commit a staged receipt: its take happened.
    pub fn commit(&self, cid: &str, id: &str) -> std::result::Result<(), String> {
        let (from, to) = (self.staged_path(cid, id), self.receipt_path(id));
        std::fs::rename(&from, &to).map_err(|e| format!("commit {}: {e}", to.display()))
    }

    /// A committed receipt's document (the take, and the tuple as taken).
    pub fn receipt_text(&self, id: &str) -> std::result::Result<String, String> {
        let path = self.receipt_path(id);
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))
    }
}

fn ids(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// A tuplespace layer: its private space and its index.
pub struct Tuplespace {
    /// The space and the receipts.
    pub store: Store,
    /// What is pending and what was taken.
    pub index: std::sync::RwLock<Index>,
}

/// What [`Tuplespace::open`] found that the feed may not have logged yet: every `out` and
/// `taken` event the stored tuples and receipts carry.
pub type Stored = Vec<Event>;

impl Tuplespace {
    /// Open a layer's space under `dir` (`<state>/{layer}`), settle any take a crash
    /// interrupted, and rebuild the index from the space, the receipts and the subjects the
    /// feed has logged (`logged`: every `out` event's IRI, so a tuple that left the space
    /// without a receipt still keeps its name used).
    pub fn open(
        def: &LayerDef,
        dir: &Path,
        logged: &[String],
    ) -> std::result::Result<(Tuplespace, Stored), String> {
        let store = Store::open(dir)?;
        let layer = def.name.as_str();
        let pending_ids: BTreeSet<String> = block_on(store.list())
            .map_err(|e| format!("list the space: {e}"))?
            .into_iter()
            .collect();
        settle_staged(&store, &pending_ids)?;
        let mut index = Index::default();
        let mut stored = Vec::new();
        for file in read_dir(&store.receipts)? {
            let name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let Some(id) = name.strip_suffix(".ttl") else {
                continue;
            };
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("read {}: {e}", file.display()))?;
            let receipt =
                parse_receipt(layer, &text).map_err(|e| format!("{}: {e}", file.display()))?;
            if receipt.id != id {
                return Err(format!(
                    "{}: holds receipt {}, which belongs in {}.ttl",
                    file.display(),
                    receipt.iri,
                    receipt.id
                ));
            }
            stored.push(out_event(&receipt.tuple));
            stored.push(taken_event(&receipt));
            index.insert_receipt(receipt)?;
        }
        for cid in &pending_ids {
            let bytes = block_on(store.read(cid))
                .map_err(|e| format!("read tuple {cid}: {e}"))?
                .ok_or_else(|| format!("tuple {cid} vanished while the layer opened"))?;
            let text = String::from_utf8(bytes).map_err(|_| format!("tuple {cid} is not UTF-8"))?;
            let triples = rdf::parse_turtle(&text).map_err(|e| format!("tuple {cid}: {e}"))?;
            let meta = tuple_meta(layer, &triples, cid).map_err(|e| format!("tuple {cid}: {e}"))?;
            stored.push(out_event(&meta));
            index.insert_pending(meta)?;
        }
        for subject in logged {
            index.subjects.entry(subject.clone()).or_insert(Place::Gone);
        }
        stored.sort_by_key(|e| e.seq);
        Ok((
            Tuplespace {
                store,
                index: std::sync::RwLock::new(index),
            },
            stored,
        ))
    }

    /// Record a drop that has been stored and logged.
    pub fn record_out(&self, meta: TupleMeta) {
        self.index
            .write()
            .expect("tuplespace lock")
            .insert_pending(meta)
            .expect("a fresh subject, checked under the layer's write lock");
    }

    /// Record a take that has been committed.
    pub fn record_taken(&self, receipt: Receipt) {
        self.index.write().expect("tuplespace lock").taken(receipt);
    }

    /// Record that a pending tuple was found gone from the space.
    pub fn record_gone(&self, subject: &str) {
        self.index.write().expect("tuplespace lock").gone(subject);
    }
}

/// Run one of the store's futures to completion outside any executor (opening is
/// synchronous). Intray's endpoint answers without waiting on anything, so this never
/// parks.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    futures::executor::block_on(future)
}

fn read_dir(dir: &Path) -> std::result::Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))? {
        let path = entry
            .map_err(|e| format!("read {}: {e}", dir.display()))?
            .path();
        if path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Settle every staged receipt: its take happened if its tuple is no longer pending.
fn settle_staged(store: &Store, pending: &BTreeSet<String>) -> std::result::Result<(), String> {
    for file in read_dir(&store.staging)? {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let Some((cid, id)) = name
            .strip_suffix(".ttl")
            .and_then(|stem| stem.split_once('.'))
        else {
            continue;
        };
        if pending.contains(cid) {
            store.unstage(cid, id);
        } else {
            store.commit(cid, id)?;
        }
    }
    Ok(())
}

/// The `out` event a stored tuple carries.
pub fn out_event(meta: &TupleMeta) -> Event {
    Event {
        seq: meta.seq,
        time: meta.created.clone(),
        kind: "out".into(),
        iri: meta.subject.clone(),
        by: meta.dropper.clone(),
    }
}

/// The `taken` event a receipt carries.
pub fn taken_event(receipt: &Receipt) -> Event {
    Event {
        seq: receipt.seq,
        time: receipt.time.clone(),
        kind: "taken".into(),
        iri: receipt.tuple.subject.clone(),
        by: receipt.taker.clone(),
    }
}

fn digest<'a>(triples: impl Iterator<Item = &'a Triple>) -> u64 {
    let mut lines: Vec<String> = triples.map(|t| t.to_string()).collect();
    lines.sort();
    lines.dedup();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    lines.hash(&mut hasher);
    hasher.finish()
}

fn is_stamp(predicate: &str) -> bool {
    refuse_stamp(predicate).is_err()
}

/// Read a stored (stamped) tuple's facts. `cid` is its intray id.
fn tuple_meta(
    layer: &str,
    triples: &[Triple],
    cid: &str,
) -> std::result::Result<TupleMeta, String> {
    let mut subject: Option<&str> = None;
    for t in triples {
        let s = subject_iri(&t.subject).ok_or("a blank-node subject")?;
        match subject {
            None => subject = Some(s),
            Some(one) if one != s => return Err("more than one subject".into()),
            _ => {}
        }
    }
    let subject = subject.ok_or("no tuple")?.to_string();
    check_subject(layer, &subject)?;
    let (mut dropper, mut created, mut seq) = (None, None, None);
    let (mut types, mut addressed_to, mut offer) = (BTreeSet::new(), BTreeSet::new(), None);
    let (addressed_p, offer_p, seq_p) = (rdf::ik_addressed_to(), rdf::ik_offer(), rdf::ik_seq());
    for t in triples {
        let p = t.predicate.as_str();
        if p == rdf::PROV_ATTRIBUTED {
            dropper = object_iri(&t.object).map(str::to_string);
        } else if p == rdf::DCT_CREATED {
            if let Term::Literal(l) = &t.object {
                created = Some(l.value().to_string());
            }
        } else if p == seq_p {
            if let Term::Literal(l) = &t.object {
                seq = l.value().parse::<u64>().ok();
            }
        } else if p == rdf::RDF_TYPE {
            types.extend(object_iri(&t.object).map(str::to_string));
        } else if p == addressed_p {
            addressed_to.extend(object_iri(&t.object).map(str::to_string));
        } else if p == offer_p {
            offer = object_iri(&t.object).map(str::to_string);
        }
    }
    Ok(TupleMeta {
        seq: seq.ok_or("no ik:seq")?,
        subject,
        cid: cid.to_string(),
        dropper: dropper.ok_or("no prov:wasAttributedTo")?,
        created: created.ok_or("no dcterms:created")?,
        types,
        addressed_to,
        offer,
        digest: digest(triples.iter().filter(|t| !is_stamp(t.predicate.as_str()))),
    })
}

/// Whether `subject` may name a tuple of `layer`: an IRI under `urn:layer:{layer}:`, not
/// beginning with a word the host's own resources use, at most [`MAX_SUBJECT`] characters.
pub fn check_subject(layer: &str, subject: &str) -> std::result::Result<(), String> {
    let prefix = format!("urn:layer:{layer}:");
    let Some(local) = subject.strip_prefix(&prefix) else {
        return Err(format!(
            "subject <{subject}>: a tuple of layer `{layer}` is named under {prefix}, e.g. \
             {prefix}offer:{{ulid}}"
        ));
    };
    let first = local.split(':').next().unwrap_or_default();
    if local.is_empty() || first.is_empty() || RESERVED.contains(&first) {
        return Err(format!(
            "subject <{subject}>: `{first}` names one of the host's own resources; name a \
             tuple e.g. {prefix}offer:{{ulid}}"
        ));
    }
    if subject.len() > MAX_SUBJECT {
        return Err(format!(
            "subject <{subject}> is longer than {MAX_SUBJECT} characters"
        ));
    }
    Ok(())
}

/// A receipt document: the take, and the tuple as taken. The tuple's intray id is not part
/// of the record (its bytes left the space with the take), so it reads back empty.
fn parse_receipt(layer: &str, text: &str) -> std::result::Result<Receipt, String> {
    let triples = rdf::parse_turtle(text)?;
    let prefix = receipt_prefix(layer);
    let (mut take, mut tuple): (Vec<Triple>, Vec<Triple>) = (Vec::new(), Vec::new());
    for t in triples {
        let is_take = subject_iri(&t.subject).is_some_and(|s| s.starts_with(&prefix));
        if is_take {
            take.push(t);
        } else {
            tuple.push(t);
        }
    }
    let iri = take
        .first()
        .and_then(|t| subject_iri(&t.subject))
        .ok_or("no receipt")?
        .to_string();
    let id = iri
        .strip_prefix(&prefix)
        .filter(|id| id::is_id(id))
        .ok_or_else(|| format!("{iri} is not a receipt of layer `{layer}`"))?
        .to_string();
    let (mut taker, mut time, mut seq, mut used) = (None, None, None, None);
    let seq_p = rdf::ik_seq();
    for t in &take {
        if subject_iri(&t.subject) != Some(iri.as_str()) {
            return Err("more than one receipt".into());
        }
        let p = t.predicate.as_str();
        if p == rdf::PROV_ASSOCIATED {
            taker = object_iri(&t.object).map(str::to_string);
        } else if p == rdf::PROV_ENDED {
            if let Term::Literal(l) = &t.object {
                time = Some(l.value().to_string());
            }
        } else if p == seq_p {
            if let Term::Literal(l) = &t.object {
                seq = l.value().parse::<u64>().ok();
            }
        } else if p == rdf::PROV_USED {
            used = object_iri(&t.object).map(str::to_string);
        }
    }
    let seq = seq.ok_or("no ik:seq")?;
    if id::seq_of(&id) != Some(seq) {
        return Err(format!("{iri} carries ik:seq {seq}, which its id does not"));
    }
    let meta = tuple_meta(layer, &tuple, "")?;
    if used.as_deref() != Some(meta.subject.as_str()) {
        return Err(format!("{iri} does not name the tuple it holds"));
    }
    Ok(Receipt {
        seq,
        id,
        iri,
        taker: taker.ok_or("no prov:wasAssociatedWith")?,
        time: time.ok_or("no prov:endedAtTime")?,
        tuple: meta,
    })
}

/// What a dropper sent to `out`, checked for shape but not yet stamped.
#[derive(Debug)]
pub struct TupleDraft {
    /// Its name.
    pub subject: String,
    triples: Vec<Triple>,
    /// The participants it is addressed to.
    pub addressed_to: BTreeSet<String>,
    /// The offer it reports on.
    pub offer: Option<String>,
    digest: u64,
}

/// Parse and check what a dropper sent to `out`: Turtle, or JSON-LD when it starts with `{`
/// or `[`.
///
/// Refused (as a bad `content`): more than the layer's `entry_max` bytes; a parse error; a
/// named graph; anything but exactly one subject, an IRI under `urn:layer:{layer}:` that is
/// not one of the host's own words ([`check_subject`]); a blank node or a quoted triple
/// anywhere; any of the three stamps (`prov:wasAttributedTo`, `dcterms:created`, `ik:seq`);
/// no `rdf:type`, or a type no shape of the layer targets; an `ik:addressedTo` or an
/// `ik:offer` that is not an IRI; more than one `ik:offer`.
pub fn draft(def: &LayerDef, content: &[u8]) -> Result<TupleDraft> {
    let triples = parse_content(def, content)?;
    let mut subject: Option<String> = None;
    for t in &triples {
        let s = match &t.subject {
            NamedOrBlankNode::NamedNode(n) => n.as_str(),
            NamedOrBlankNode::BlankNode(_) => {
                return Err(invalid(
                    "a blank-node subject: a tuple is named by its dropper, under \
                     urn:layer:{layer}:",
                ))
            }
        };
        match &subject {
            None => subject = Some(s.to_string()),
            Some(one) if one != s => {
                return Err(invalid("more than one subject: a tuple is one subject"))
            }
            _ => {}
        }
    }
    let subject = subject.expect("parse_content refuses an empty graph");
    check_subject(&def.name, &subject).map_err(invalid)?;
    let (addressed_p, offer_p) = (rdf::ik_addressed_to(), rdf::ik_offer());
    let (mut types, mut addressed_to, mut offers) =
        (BTreeSet::new(), BTreeSet::new(), BTreeSet::new());
    for t in &triples {
        let p = t.predicate.as_str();
        refuse_stamp(p)?;
        if !matches!(t.object, Term::NamedNode(_) | Term::Literal(_)) {
            return Err(invalid(
                "a blank node or a quoted triple as an object: a tuple is one subject, and \
                 every other node is named by an IRI",
            ));
        }
        if p == rdf::RDF_TYPE {
            types.insert(admitted_type(def, &t.object)?);
        } else if p == addressed_p {
            let to = object_iri(&t.object)
                .ok_or_else(|| invalid("ik:addressedTo names a participant by its IRI"))?;
            addressed_to.insert(to.to_string());
        } else if p == offer_p {
            let offer = object_iri(&t.object)
                .ok_or_else(|| invalid("ik:offer names an offer by its IRI"))?;
            offers.insert(offer.to_string());
        }
    }
    require_type(def, &types)?;
    if offers.len() > 1 {
        return Err(invalid(
            "more than one ik:offer: a report is about one offer",
        ));
    }
    Ok(TupleDraft {
        digest: digest(triples.iter()),
        subject,
        triples,
        addressed_to,
        offer: offers.into_iter().next(),
    })
}

impl TupleDraft {
    /// Whether `meta` is this same drop again from `dropper`: the same subject, dropper and
    /// content. A replay (a retry after a lost reply) is answered as the drop it repeats.
    pub fn replays(&self, meta: &TupleMeta, dropper: &str) -> bool {
        meta.subject == self.subject && meta.dropper == dropper && meta.digest == self.digest
    }

    /// The tuple as it will be stored, attributed, dated and numbered.
    pub fn stamp(&self, dropper: &str, created: &str, seq: u64) -> Vec<Triple> {
        let subject = node(&self.subject);
        let mut stamped = self.triples.clone();
        stamped.push(Triple::new(
            subject.clone(),
            node(rdf::PROV_ATTRIBUTED),
            node(dropper),
        ));
        stamped.push(Triple::new(
            subject.clone(),
            node(rdf::DCT_CREATED),
            Literal::new_typed_literal(created, node(rdf::XSD_DATE_TIME)),
        ));
        stamped.push(Triple::new(
            subject,
            node(&rdf::ik_seq()),
            Literal::new_typed_literal(seq.to_string(), node(rdf::XSD_INTEGER)),
        ));
        stamped
    }

    /// The index entry for this draft once stamped and stored under `cid`.
    pub fn meta(&self, cid: &str, dropper: &str, created: &str, seq: u64) -> TupleMeta {
        TupleMeta {
            seq,
            subject: self.subject.clone(),
            cid: cid.to_string(),
            dropper: dropper.to_string(),
            created: created.to_string(),
            types: self
                .triples
                .iter()
                .filter(|t| t.predicate.as_str() == rdf::RDF_TYPE)
                .filter_map(|t| object_iri(&t.object).map(str::to_string))
                .collect(),
            addressed_to: self.addressed_to.clone(),
            offer: self.offer.clone(),
            digest: self.digest,
        }
    }
}

/// A receipt's document: the take (a `prov:Activity` that used the tuple, associated with
/// its taker, ended at `time`, numbered `seq` in the feed), then the tuple as taken.
pub fn receipt_document(
    layer: &str,
    id: &str,
    taker: &str,
    time: &str,
    seq: u64,
    subject: &str,
    tuple: &[Triple],
) -> (String, String) {
    let iri = format!("{}{id}", receipt_prefix(layer));
    let s = node(&iri);
    let mut triples = vec![
        Triple::new(s.clone(), node(rdf::RDF_TYPE), node(rdf::PROV_ACTIVITY)),
        Triple::new(s.clone(), node(rdf::PROV_USED), node(subject)),
        Triple::new(s.clone(), node(rdf::PROV_ASSOCIATED), node(taker)),
        Triple::new(
            s.clone(),
            node(rdf::PROV_ENDED),
            Literal::new_typed_literal(time, node(rdf::XSD_DATE_TIME)),
        ),
        Triple::new(
            s,
            node(&rdf::ik_seq()),
            Literal::new_typed_literal(seq.to_string(), node(rdf::XSD_INTEGER)),
        ),
    ];
    triples.extend(tuple.iter().cloned());
    (iri, rdf::to_turtle(&triples))
}

/// Build the [`Receipt`] for a take whose document is `text`.
pub fn receipt_of(layer: &str, text: &str) -> Result<Receipt> {
    parse_receipt(layer, text).map_err(Error::Endpoint)
}
