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
//!   it reaches the offerer and nobody else, exactly once;
//! - **leases** (ledger #2119): in a layer defined with `lease_max`, a `take lease=` is a
//!   claim with a deadline rather than a final take. Its taker settles it (`done`, naming an
//!   optional result) or renews it before it lapses; a lapsed lease returns the tuple to the
//!   space (the same tuple, under the same subject and `ik:seq`, with a `returned` feed event),
//!   and the next take's receipt counts the attempt (`layers:attempt`). A settle after the lapse
//!   is refused, so a tuple has at most one result. A take with no `lease=` stays final unless
//!   the layer sets `lease_required = true`, in which case it is refused.
//!
//! # Crash order
//!
//! A take stages its receipt (`receipts/.taking/{cid}.{id}.ttl`), then takes from intray,
//! then commits the receipt (a rename), then logs the feed event. On open, a staged receipt
//! whose tuple is still pending is a take that never happened (removed); one whose tuple
//! is gone is a take that did (committed), and one that does not read back whole is a stage
//! that never completed (discarded). Events stored but not logged are logged then, in order,
//! as the channel model does. A take whose commit fails is still a take: it is recorded, its
//! number is spent, and its receipt is read from where it is staged until that open.
//!
//! A receipt never changes; a leased take's standing since (renewed, done or lapsed) is a
//! lease file beside it, `<state>/{layer}/leases/{id}.json`, replaced whole. A lapse writes
//! the lapse (and its feed number) there FIRST, then drops the tuple back into the space, then
//! logs the `returned` event. An open that finds a lapse whose tuple is not back drops it
//! again from the receipt, which holds the tuple as taken; one whose tuple is back and taken
//! again keeps both receipts, the lapsed one and the new one.
//!
//! # Sealed at rest
//!
//! Every tuple is sealed ([`crate::at_rest`]) by intray itself, through the host's
//! [`TupleCodec`] (ikigai-intray 0.1.69, ledger #2017): `out` seals, and `rd`, `take` and every
//! `match=` open, so the intray id is the hash of the ciphertext and a template is matched
//! against the opened Turtle by intray's own evaluator, within the host's one budget. A pending
//! tuple that does not open refuses a `match=` with intray's typed `Conflict` naming it, never
//! reads as absent. Every receipt, staged or committed, is sealed too, by this module.
//!
//! At open, before anything is served, every staged drop and claim intray left under the space
//! is discarded ([`SpaceEndpoint::discard_staging`]): the host holds the state directory, so
//! none is live. A staged drop was never published, and a staged claim is a take whose receipt
//! the open then commits; left in place, intray would put a claim back in the inbox after five
//! minutes, pending again beside its receipt.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ikigai_core::{
    ArgRef, Bindings, Capability, Endpoint, Error, Invocation, Iri, Request, Result, Verb,
};
use ikigai_intray::{
    tuple_id, SpaceEndpoint, TupleCodec, ERROR_DIR, INBOX_DIR, OUTBOX_DIR, PROCESSING_DIR,
    SPACE_DIRS,
};
use ikigai_store::budget::EvaluationBudget;
use oxrdf::{Literal, NamedOrBlankNode, Term, Triple};

use crate::at_rest::{self, Sealer, Unsealed};

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
    /// Its intray id while it is pending: the blake3 of its STORED bytes, the stamped Turtle as
    /// sealed (intray names a tuple by what its codec stored), so it changes when the tuple is
    /// sealed again ([`crate::Migration::Reseal`]). Empty once taken, when its bytes have left
    /// the space.
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
    /// How many leased takes of it have lapsed: 0 for a tuple never returned to the space.
    pub attempts: u64,
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
    /// Which take of the tuple this is, from 1 ([`TupleMeta::attempts`] + 1).
    pub attempt: u64,
    /// The lease, for a `take lease=`; `None` for a final take.
    pub lease: Option<Lease>,
}

impl Receipt {
    /// Whether this take lapsed and its tuple went (or is going) back to the space.
    pub fn lapsed(&self) -> bool {
        matches!(
            self.lease,
            Some(Lease {
                state: LeaseState::Lapsed { .. },
                ..
            })
        )
    }
}

/// A leased take: until when it holds, and how it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    /// When it lapses unless settled or renewed, as `xsd:dateTime`.
    pub until: String,
    /// How it stands.
    pub state: LeaseState,
}

impl Lease {
    /// Whether a read at `now` computes this lease as lapsed while no lapse is recorded yet:
    /// it is held and its `until` has passed. A read never records it (ledger #4075); the
    /// lease sweep, or the next write on the layer, does.
    pub fn lapsed_unrecorded(&self, now: Option<&str>) -> bool {
        self.state == LeaseState::Held && now.is_some_and(|now| passed(&self.until, now))
    }

    /// Its standing's word as a read at `now` sees it: `lapsed` for a held lease past its
    /// `until`, whether or not the lapse is recorded yet.
    pub fn word_at(&self, now: Option<&str>) -> &'static str {
        if self.lapsed_unrecorded(now) {
            "lapsed"
        } else {
            self.state.word()
        }
    }

    /// For a lease that reads as lapsed at `now`, whether its lapse is recorded (its lease file
    /// and the `returned` event written); `None` for one that does not read as lapsed.
    pub fn recorded_at(&self, now: Option<&str>) -> Option<bool> {
        match self.state {
            LeaseState::Lapsed { .. } => Some(true),
            _ if self.lapsed_unrecorded(now) => Some(false),
            _ => None,
        }
    }
}

/// A lease's standing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseState {
    /// Its taker holds the tuple until [`Lease::until`].
    Held,
    /// Its taker settled it, naming `result` if it gave one: the take is final.
    Done {
        /// The result reference (an IRI) its taker named.
        result: Option<String>,
    },
    /// It lapsed at `time`, its lease's `until` (ledger #4075), and the tuple went back to the
    /// space with the `returned` event numbered `seq`.
    Lapsed {
        /// The `returned` event's sequence number.
        seq: u64,
        /// When it lapsed, as `xsd:dateTime`.
        time: String,
    },
}

impl LeaseState {
    /// Its word, as `layers:leaseState` carries it.
    pub fn word(&self) -> &'static str {
        match self {
            LeaseState::Held => "held",
            LeaseState::Done { .. } => "done",
            LeaseState::Lapsed { .. } => "lapsed",
        }
    }
}

/// A lease as its file stores it.
#[derive(serde::Serialize, serde::Deserialize)]
struct LeaseFile {
    until: String,
    state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    time: Option<String>,
}

impl LeaseFile {
    fn of(lease: &Lease) -> LeaseFile {
        let (result, seq, time) = match &lease.state {
            LeaseState::Held => (None, None, None),
            LeaseState::Done { result } => (result.clone(), None, None),
            LeaseState::Lapsed { seq, time } => (None, Some(*seq), Some(time.clone())),
        };
        LeaseFile {
            until: lease.until.clone(),
            state: lease.state.word().into(),
            result,
            seq,
            time,
        }
    }

    fn lease(self) -> std::result::Result<Lease, String> {
        let state = match (self.state.as_str(), self.seq, self.time) {
            ("held", _, _) => LeaseState::Held,
            ("done", _, _) => LeaseState::Done {
                result: self.result,
            },
            ("lapsed", Some(seq), Some(time)) => LeaseState::Lapsed { seq, time },
            (other, _, _) => return Err(format!("`{other}` is not a lease state it can hold")),
        };
        Ok(Lease {
            until: self.until,
            state,
        })
    }
}

/// Whether `until` is at or before `now`, both `xsd:dateTime`. One that does not parse (never
/// written by this host) counts as passed, so a lease never holds a tuple forever.
pub fn passed(until: &str, now: &str) -> bool {
    use std::str::FromStr;
    match (
        oxsdatatypes::DateTime::from_str(until),
        oxsdatatypes::DateTime::from_str(now),
    ) {
        (Ok(until), Ok(now)) => until <= now,
        _ => true,
    }
}

/// A lease's `until` in milliseconds since the epoch, for the index of held leases. One that
/// does not parse, or is before the epoch, is 0: due at once, as [`passed`] counts it passed.
pub(crate) fn until_millis(until: &str) -> u64 {
    use std::str::FromStr;
    let epoch = oxsdatatypes::DateTime::from_str("1970-01-01T00:00:00Z").expect("the epoch");
    oxsdatatypes::DateTime::from_str(until)
        .ok()
        .and_then(|until| until.checked_sub(epoch))
        .and_then(|since| std::time::Duration::try_from(since).ok())
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

/// The order two leases lapse in, by their `until` instants. One that does not parse sorts
/// first, as [`passed`] counts it passed.
fn lapse_order(a: &str, b: &str) -> std::cmp::Ordering {
    use std::str::FromStr;
    let at = |t: &str| oxsdatatypes::DateTime::from_str(t).ok();
    match (at(a), at(b)) {
        (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
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
    /// The receipts whose lease is held, or lapsed with its tuple not yet back in the space.
    live: BTreeSet<String>,
    /// Every HELD lease, by its `until` (in milliseconds, [`until_millis`]) and then its
    /// receipt's id: the first is the next lapse due ([`Index::next_lapse`]). Kept beside
    /// `live` as each lease is granted, renewed, settled or recorded lapsed, so the answer is
    /// O(log n) to maintain and never a scan (ledger #4077).
    held: BTreeSet<(u64, String)>,
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

    fn insert_pending(&mut self, mut meta: TupleMeta) -> std::result::Result<(), String> {
        match self.subjects.get(&meta.subject) {
            None => {}
            // Back in the space after a lapse: the lapsed receipt counts its attempt.
            Some(Place::Taken(i)) if self.receipts[*i].lapsed() => {
                let lapsed = &self.receipts[*i];
                meta.attempts = lapsed.attempt;
                self.live.remove(&lapsed.id);
            }
            Some(_) => return Err(format!("{} is stored twice", meta.subject)),
        }
        self.subjects
            .insert(meta.subject.clone(), Place::Pending(meta.seq));
        self.by_cid.insert(meta.cid.clone(), meta.seq);
        self.pending.insert(meta.seq, meta);
        Ok(())
    }

    fn insert_receipt(&mut self, receipt: Receipt) -> std::result::Result<(), String> {
        match self.subjects.get(&receipt.tuple.subject) {
            None => {}
            // Taken again after a lapse: both receipts stay, and the new one is current.
            Some(Place::Taken(i))
                if self.receipts[*i].lapsed() && self.receipts[*i].seq < receipt.seq =>
            {
                let lapsed = self.receipts[*i].id.clone();
                self.live.remove(&lapsed);
            }
            Some(_) => return Err(format!("{} is stored twice", receipt.tuple.subject)),
        }
        if matches!(
            receipt.lease,
            Some(Lease {
                state: LeaseState::Held | LeaseState::Lapsed { .. },
                ..
            })
        ) {
            self.live.insert(receipt.id.clone());
        }
        self.hold(&receipt.id, receipt.lease.as_ref());
        let i = self.receipts.len();
        self.subjects
            .insert(receipt.tuple.subject.clone(), Place::Taken(i));
        self.by_receipt_id.insert(receipt.id.clone(), i);
        self.receipts.push(receipt);
        Ok(())
    }

    /// A pending tuple stored again under a new intray id (sealed again by
    /// [`crate::Migration::Reseal`]).
    fn recid(&mut self, seq: u64, cid: String) {
        if let Some(meta) = self.pending.get_mut(&seq) {
            self.by_cid.remove(&meta.cid);
            self.by_cid.insert(cid.clone(), seq);
            meta.cid = cid;
        }
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

    /// The receipts a lease sweep at `now` acts on, in lapse order (`until`, then take order):
    /// a held lease that has lapsed, and a lapse whose tuple is not back in the space yet. The
    /// sweep records each at its `until`, so in this order the `returned` times it logs never
    /// go backward.
    pub fn due(&self, now: &str) -> Vec<Receipt> {
        let mut due: Vec<Receipt> = self
            .live
            .iter()
            .filter_map(|id| self.receipt(id))
            .filter(|r| match &r.lease {
                Some(lease) => match lease.state {
                    LeaseState::Held => passed(&lease.until, now),
                    LeaseState::Lapsed { .. } => true,
                    LeaseState::Done { .. } => false,
                },
                None => false,
            })
            .cloned()
            .collect();
        let until = |r: &Receipt| {
            r.lease
                .as_ref()
                .map(|l| l.until.clone())
                .unwrap_or_default()
        };
        due.sort_by(|a, b| lapse_order(&until(a), &until(b)).then(a.seq.cmp(&b.seq)));
        due
    }

    /// The receipts whose take a read at `now` computes as over though their tuple is not back
    /// in the space yet ([`Index::returning`]): their tuples read as pending, from the receipt
    /// (ledger #4075).
    pub fn lapsed_unrecorded<'a>(
        &'a self,
        now: Option<&'a str>,
    ) -> impl Iterator<Item = &'a Receipt> {
        self.live
            .iter()
            .filter_map(|id| self.receipt(id))
            .filter(move |r| self.returning(r, now))
    }

    /// Whether `receipt`'s lease is over at `now` while its tuple is not back in the space: a
    /// held lease past its `until` with no lapse recorded yet ([`Lease::lapsed_unrecorded`]),
    /// or a lapse recorded whose drop back has not happened yet (it failed, and the next write
    /// or sweep drops it again). Either way the take is over and the tuple reads as pending.
    pub fn returning(&self, receipt: &Receipt, now: Option<&str>) -> bool {
        self.live.contains(&receipt.id)
            && receipt.lease.as_ref().is_some_and(|l| {
                l.lapsed_unrecorded(now) || matches!(l.state, LeaseState::Lapsed { .. })
            })
    }

    /// The receipt `subject` is taken under now, if it is taken.
    pub fn current(&self, subject: &str) -> Option<&Receipt> {
        match self.subjects.get(subject) {
            Some(Place::Taken(i)) => Some(&self.receipts[*i]),
            _ => None,
        }
    }

    /// A receipt's lease as it stands now.
    fn set_lease(&mut self, id: &str, lease: Lease) {
        let Some(i) = self.by_receipt_id.get(id).copied() else {
            return;
        };
        if matches!(lease.state, LeaseState::Done { .. }) {
            self.live.remove(id);
        }
        if let Some(old) = self.receipts[i].lease.as_ref() {
            self.held
                .remove(&(until_millis(&old.until), id.to_string()));
        }
        self.hold(id, Some(&lease));
        self.receipts[i].lease = Some(lease);
    }

    /// Index `lease` under receipt `id` when it is held.
    fn hold(&mut self, id: &str, lease: Option<&Lease>) {
        if let Some(lease) = lease.filter(|l| l.state == LeaseState::Held) {
            self.held
                .insert((until_millis(&lease.until), id.to_string()));
        }
    }

    /// The earliest `until` of a held lease in this layer: when its next lapse is due, read
    /// from the index of held leases, not by a scan (ledger #4077). `None` when no lease is
    /// held. A lease already past its `until` and not yet recorded is still held, so the
    /// answer may be in the past: the lapse is due now.
    pub fn next_lapse(&self) -> Option<ikigai_core::Time> {
        self.held
            .first()
            .map(|(millis, _)| ikigai_core::Time::from_millis(*millis))
    }

    /// Whether `receipt` is still in play, so retention keeps it whatever its age: a held
    /// lease is not over yet (computed lapsed or not, until its lapse is recorded and its tuple
    /// is back), and a lapsed one counts the attempts of a tuple pending again.
    fn in_play(&self, receipt: &Receipt) -> bool {
        self.live.contains(&receipt.id)
            || (receipt.lapsed()
                && matches!(
                    self.subjects.get(&receipt.tuple.subject),
                    Some(Place::Pending(_))
                ))
    }

    /// Whether a read COMPUTES `receipt` as expired by retention, with `before` the cutoff its
    /// instant gives (`now - retain`), though no `sweep` has recorded it yet (ledger #4082): it
    /// is out of play and was taken strictly before the cutoff, the rule a sweep deletes by. A
    /// read does not show such a receipt and never writes. `None` (no window, or no clock)
    /// computes nothing.
    pub fn expired_at(&self, receipt: &Receipt, before: Option<&str>) -> bool {
        use std::str::FromStr;
        let Some(before) = before else {
            return false;
        };
        if self.in_play(receipt) {
            return false;
        }
        match (
            oxsdatatypes::DateTime::from_str(&receipt.time),
            oxsdatatypes::DateTime::from_str(before),
        ) {
            (Ok(taken), Ok(cutoff)) => taken < cutoff,
            _ => false,
        }
    }

    /// When retention next expires a receipt under a window of `retain`: the earliest take,
    /// among the receipts out of play, plus the window and one millisecond (a receipt expires
    /// once its take is strictly before `now - retain`), or `None` when no receipt can expire
    /// (ledger #4082). An instant at or before now means an expiry is due already, recorded by
    /// the next sweep or write. A scan of the layer's receipts.
    pub fn next_expiry(&self, retain: std::time::Duration) -> Option<ikigai_core::Time> {
        let window = u64::try_from(retain.as_millis()).unwrap_or(u64::MAX);
        self.receipts
            .iter()
            .filter(|r| !self.in_play(r))
            .map(|r| until_millis(&r.time))
            .min()
            .map(|taken| {
                ikigai_core::Time::from_millis(taken.saturating_add(window).saturating_add(1))
            })
    }

    /// A lapsed take's tuple is back in the space as `meta`.
    fn returned(&mut self, id: &str, meta: TupleMeta) {
        self.live.remove(id);
        self.subjects
            .insert(meta.subject.clone(), Place::Pending(meta.seq));
        self.by_cid.insert(meta.cid.clone(), meta.seq);
        self.pending.insert(meta.seq, meta);
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
    /// `<state>/{layer}/tuples/`, the space's own directory.
    tuples: PathBuf,
    receipts: PathBuf,
    staging: PathBuf,
    /// `<state>/{layer}/leases/`: a leased take's standing since its receipt.
    leases: PathBuf,
    authority: Capability,
    /// Seals the receipts; the space seals the tuples with a clone ([`SealCodec`]).
    sealer: Sealer,
}

/// The host's [`Sealer`] as intray's [`TupleCodec`]: a tuple is sealed as intray stores it and
/// opened wherever intray reads it, and a tuple not sealed to the current identity is what
/// [`SpaceEndpoint::reencode`] seals again. A migrating sealer passes plaintext through, so a
/// [`crate::Migration::Reseal`] reads the tuples it seals.
struct SealCodec(Sealer);

impl TupleCodec for SealCodec {
    fn encode(&self, plain: &[u8]) -> std::result::Result<Vec<u8>, String> {
        self.0.seal(plain)
    }

    fn decode(&self, stored: &[u8]) -> std::result::Result<Vec<u8>, String> {
        // Intray's refusal names the space and the tuple; this is the reason inside it.
        self.0
            .open(stored)
            .map_err(|why| why.about(Path::new("the stored tuple")))
    }

    fn is_current(&self, stored: &[u8]) -> bool {
        self.0.is_current(stored)
    }
}

fn space_iri() -> Iri {
    Iri::parse(format!("urn:space:{SPACE_NAME}")).expect("a constant IRI")
}

impl Store {
    // Unit tests can hold a real match inside decoding without changing production codecs.
    #[cfg(test)]
    pub(crate) fn set_test_codec(&mut self, codec: Arc<dyn TupleCodec>) {
        let space = std::mem::replace(&mut self.space, SpaceEndpoint::new(PathBuf::new()));
        self.space = space.with_codec(codec);
    }

    /// The space under `dir`, its `match=` evaluations run within `budget`: the HOST's one,
    /// shared by every tuplespace layer, so one overdue count caps them all (ledger #1362).
    /// Every staged drop and claim intray left is discarded before anything reads the space
    /// (see the module's "Sealed at rest"; ledger #2017).
    fn open(
        dir: &Path,
        budget: EvaluationBudget,
        sealer: &Sealer,
    ) -> std::result::Result<Store, String> {
        let receipts = dir.join("receipts");
        let staging = receipts.join(".taking");
        at_rest::private_dir(&staging)?;
        let leases = dir.join("leases");
        at_rest::private_dir(&leases)?;
        let tuples = dir.join(SPACE_NAME);
        // Intray creates these with the process's umask when it first needs them; made here
        // first, they are `0700` like every other directory of the host's state.
        for stage in SPACE_DIRS {
            at_rest::private_dir(&tuples.join(stage))?;
        }
        let space = SpaceEndpoint::new(dir.to_path_buf())
            .with_evaluation_budget(budget)
            .with_codec(Arc::new(SealCodec(sealer.clone())));
        // Before the first `rd`: intray puts a claim older than five minutes back in the inbox
        // on every `rd` and `take`, and the open's own listing is one.
        space
            .discard_staging(SPACE_NAME)
            .map_err(|e| format!("clear {}'s staging: {e}", tuples.display()))?;
        Ok(Store {
            space,
            tuples,
            receipts,
            staging,
            leases,
            sealer: sealer.clone(),
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

    /// Drop `bytes` into the space, which seals them; answers the intray id (the hash of the
    /// sealed bytes).
    pub async fn drop_tuple(&self, bytes: &[u8]) -> Result<String> {
        let id = self.call(Verb::Sink, &[("content", bytes)]).await?;
        let id = String::from_utf8(id)
            .map_err(|_| Error::Endpoint("intray answered a non-UTF-8 id".into()))?;
        // Intray writes the file with the process's umask; the bytes are sealed and the
        // directory is `0700`, and the file is made `0600` too. Best effort: the drop is
        // stored either way, and the host's next open makes every file `0600`.
        let _ = at_rest::private(&self.tuple_path(&id));
        Ok(id)
    }

    fn tuple_path(&self, cid: &str) -> PathBuf {
        self.tuples.join(INBOX_DIR).join(format!("{cid}.tuple"))
    }

    /// A pending tuple's bytes, opened, or `None` when it is not in the space. One that does
    /// not open is intray's `Conflict` naming it.
    pub async fn read(&self, cid: &str) -> Result<Option<Vec<u8>>> {
        match self.call(Verb::Source, &[("tuple", cid.as_bytes())]).await {
            Ok(bytes) => Ok(Some(bytes)),
            Err(Error::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Take a tuple by its intray id: its bytes, opened, or `None` when another taker won it.
    /// One that does not open is intray's `Conflict` and stays in the space.
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

    /// The intray ids of the pending tuples a SPARQL ASK holds for: intray's own `rd match=`,
    /// which opens each tuple through the codec inside the host's one budget, with every bound
    /// intray keeps. A pending tuple that does not open refuses the match (intray's typed
    /// `Conflict` naming it), never reads as absent.
    pub async fn matching(&self, query: &str) -> Result<Vec<String>> {
        self.matching_with(query, Vec::new())
    }

    /// [`matching`](Self::matching), with `candidates` considered beside the stored tuples:
    /// each an intray id and its plaintext Turtle, never stored. Intray's one evaluation of
    /// the template covers both, within the same budget and with the same nesting bound, and
    /// answers the matching ids in id order, each once (ledger #4081). This is how `rd match=`
    /// sees a tuple whose lease lapsed unrecorded: its bytes are in its receipt, not in intray.
    pub fn matching_with(
        &self,
        query: &str,
        candidates: Vec<(String, Vec<u8>)>,
    ) -> Result<Vec<String>> {
        let request = Request::new(Verb::Source, space_iri())
            .with_arg("match", ArgRef::Inline(query.as_bytes().to_vec()));
        let mut bindings = Bindings::new();
        bindings.insert("name", SPACE_NAME);
        let inv = Invocation::detached(&request, &bindings, &self.authority);
        let bytes = self
            .space
            .invoke_with_match_candidates(&inv, candidates)?
            .bytes;
        Ok(ids(&bytes))
    }

    fn receipt_path(&self, id: &str) -> PathBuf {
        self.receipts.join(format!("{id}.ttl"))
    }

    fn staged_path(&self, cid: &str, id: &str) -> PathBuf {
        self.staging.join(format!("{cid}.{id}.ttl"))
    }

    /// Stage a receipt before its take: written once and synced. A write that fails part-way
    /// removes what it began (ledger #1221); if even that fails, or the host stops mid-write,
    /// the next open discards the torn file, since no take ever follows a stage that did not
    /// complete.
    pub fn stage(&self, cid: &str, id: &str, text: &str) -> std::result::Result<(), String> {
        let path = self.staged_path(cid, id);
        let sealed = self.sealer.seal(text.as_bytes())?;
        let mut file = at_rest::private_file()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("create {}: {e}", path.display()))?;
        let written = file.write_all(&sealed).and_then(|()| file.sync_all());
        drop(file);
        written.map_err(|e| {
            let _ = std::fs::remove_file(&path);
            format!("write {}: {e}", path.display())
        })
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

    /// A receipt's document (the take, and the tuple as taken): committed, or still staged
    /// when its take happened but the commit failed (ledger #1221), until the next open
    /// commits it.
    pub fn receipt_text(&self, id: &str) -> std::result::Result<String, String> {
        let path = self.receipt_path(id);
        let committed = match std::fs::read(&path) {
            Ok(bytes) => {
                let opened = self.sealer.open(&bytes).map_err(|why| why.about(&path))?;
                return String::from_utf8(opened)
                    .map_err(|_| format!("{} is not UTF-8", path.display()));
            }
            Err(e) => format!("read {}: {e}", path.display()),
        };
        let suffix = format!(".{id}.ttl");
        let staged = std::fs::read_dir(&self.staging).ok().and_then(|dir| {
            dir.filter_map(|entry| entry.ok().map(|e| e.path()))
                .find(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(&suffix))
                })
        });
        match staged {
            Some(staged) => self.sealer.read_text(&staged),
            None => Err(committed),
        }
    }
}

impl Store {
    fn lease_path(&self, id: &str) -> PathBuf {
        self.leases.join(format!("{id}.json"))
    }

    /// Store a leased take's standing, sealed and replaced whole.
    pub fn write_lease(&self, id: &str, lease: &Lease) -> std::result::Result<(), String> {
        let json = serde_json::to_vec(&LeaseFile::of(lease))
            .map_err(|e| format!("lease {id} did not serialize: {e}"))?;
        at_rest::replace(&self.lease_path(id), &self.sealer.seal(&json)?)
    }

    /// A leased take's standing since its receipt, if it has changed.
    fn read_lease(&self, id: &str) -> std::result::Result<Option<Lease>, String> {
        let path = self.lease_path(id);
        if !path.is_file() {
            return Ok(None);
        }
        let text = self.sealer.read_text(&path)?;
        let file: LeaseFile =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        file.lease()
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display()))
    }

    /// A receipt as it is answered: its document, with a leased take's standing as it is
    /// now (`layers:leaseUntil`, `layers:leaseState` and any `layers:leaseResult`) in place of
    /// the lease the document was written with. A held lease past its `until` at `now` reads
    /// as `lapsed`, recorded or not (ledger #4075).
    pub fn receipt_answer(
        &self,
        receipt: &Receipt,
        now: Option<&str>,
    ) -> std::result::Result<String, String> {
        let text = self.receipt_text(&receipt.id)?;
        let Some(lease) = &receipt.lease else {
            return Ok(text);
        };
        let until = rdf::layers_lease_until();
        let mut triples: Vec<Triple> = rdf::parse_turtle(&text)?
            .into_iter()
            .filter(|t| t.predicate.as_str() != until)
            .collect();
        let s = node(&receipt.iri);
        triples.push(Triple::new(
            s.clone(),
            node(&until),
            Literal::new_typed_literal(&lease.until, node(rdf::XSD_DATE_TIME)),
        ));
        triples.push(Triple::new(
            s.clone(),
            node(&rdf::layers_lease_state()),
            Literal::new_simple_literal(lease.word_at(now)),
        ));
        if let LeaseState::Done {
            result: Some(result),
        } = &lease.state
        {
            triples.push(Triple::new(
                s,
                node(&rdf::layers_lease_result()),
                node(result),
            ));
        }
        Ok(rdf::to_turtle(&triples))
    }

    /// The tuple a receipt holds, as it was taken.
    pub(crate) fn taken_tuple(
        &self,
        receipt: &Receipt,
    ) -> std::result::Result<Vec<Triple>, String> {
        let text = self.receipt_text(&receipt.id)?;
        Ok(rdf::parse_turtle(&text)?
            .into_iter()
            .filter(|t| subject_iri(&t.subject) != Some(receipt.iri.as_str()))
            .collect())
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
    /// without a receipt still keeps its name used). Its `match=` evaluations run within a
    /// budget of its OWN ([`EvaluationBudget::default`]); a host opens every layer with
    /// [`open_with_budget`](Self::open_with_budget) instead, so they share one.
    pub fn open(
        def: &LayerDef,
        dir: &Path,
        logged: &[String],
        sealer: &Sealer,
    ) -> std::result::Result<(Tuplespace, Stored), String> {
        Self::open_with_budget(def, dir, logged, EvaluationBudget::default(), sealer)
    }

    /// [`open`](Self::open), its `match=` evaluations within `budget`: a clone of the host's
    /// one, so every layer shares one overdue count (see [`crate::Host::match_budget`],
    /// ledger #1362).
    ///
    /// Two pending tuples that open to the same bytes are one tuple stored twice (a
    /// [`crate::Migration::Reseal`] stores the sealed copy before it removes the old one, and
    /// may stop between the two; intray's re-encoding stages the old one as a claim, which this
    /// open discards, but a host before it took the old copy as an ordinary take): one is kept
    /// and the other taken out of the space.
    pub fn open_with_budget(
        def: &LayerDef,
        dir: &Path,
        logged: &[String],
        budget: EvaluationBudget,
        sealer: &Sealer,
    ) -> std::result::Result<(Tuplespace, Stored), String> {
        let store = Store::open(dir, budget, sealer)?;
        let layer = def.name.as_str();
        let pending_ids: BTreeSet<String> = block_on(store.list())
            .map_err(|e| format!("list the space: {e}"))?
            .into_iter()
            .collect();
        settle_staged(&store, &pending_ids, layer)?;
        let mut index = Index::default();
        let mut stored = Vec::new();
        let mut receipts = Vec::new();
        for file in read_dir(&store.receipts)? {
            let name = file
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let Some(id) = name.strip_suffix(".ttl") else {
                continue;
            };
            let text = sealer.read_text(&file)?;
            let mut receipt =
                parse_receipt(layer, &text).map_err(|e| format!("{}: {e}", file.display()))?;
            if receipt.id != id {
                return Err(format!(
                    "{}: holds receipt {}, which belongs in {}.ttl",
                    file.display(),
                    receipt.iri,
                    receipt.id
                ));
            }
            if let Some(lease) = store.read_lease(id)? {
                receipt.lease = Some(lease);
            }
            stored.push(out_event(&receipt.tuple));
            stored.push(taken_event(&receipt));
            stored.extend(returned_event(&receipt));
            receipts.push(receipt);
        }
        // In take order, so a tuple taken again after a lapse is current under its new take.
        receipts.sort_by_key(|r| r.seq);
        for receipt in receipts {
            index.insert_receipt(receipt)?;
        }
        let mut texts: HashMap<String, String> = HashMap::new();
        for cid in &pending_ids {
            let bytes = block_on(store.read(cid))
                .map_err(|e| format!("read tuple {cid}: {e}"))?
                .ok_or_else(|| format!("tuple {cid} vanished while the layer opened"))?;
            let text = String::from_utf8(bytes).map_err(|_| format!("tuple {cid} is not UTF-8"))?;
            let triples = rdf::parse_turtle(&text).map_err(|e| format!("tuple {cid}: {e}"))?;
            let meta = tuple_meta(layer, &triples, cid).map_err(|e| format!("tuple {cid}: {e}"))?;
            if texts.get(&meta.subject).is_some_and(|seen| *seen == text) {
                // The same tuple stored twice (see above): keep the first.
                block_on(store.take(cid)).map_err(|e| format!("take duplicate {cid}: {e}"))?;
                continue;
            }
            texts.insert(meta.subject.clone(), text);
            stored.push(out_event(&meta));
            index.insert_pending(meta)?;
        }
        for subject in logged {
            index.subjects.entry(subject.clone()).or_insert(Place::Gone);
        }
        // A tuple taken more than once carries its one `out` event in each receipt.
        stored.sort_by_key(|e| e.seq);
        stored.dedup_by_key(|e| e.seq);
        let space = Tuplespace {
            store,
            index: std::sync::RwLock::new(index),
        };
        // A lapse whose tuple is not back (the host stopped between the two): drop it again.
        let due: Vec<Receipt> = {
            let index = space.index.read().expect("tuplespace lock");
            index
                .live
                .iter()
                .filter_map(|id| index.receipt(id))
                .filter(|r| r.lapsed())
                .cloned()
                .collect()
        };
        for receipt in due {
            block_on(space.return_tuple(layer, &receipt))?;
        }
        Ok((space, stored))
    }

    /// Seal again, to the current identity, everything this layer's space holds that is
    /// plaintext or sealed only to an older identity ([`crate::Migration::Reseal`]):
    ///
    /// - each receipt, committed or staged, replaced whole;
    /// - each pending tuple, STORED AGAIN by intray ([`SpaceEndpoint::reencode`]): sealed bytes
    ///   are a new intray id, and the index follows each one to its new id, keeping its seq. A
    ///   stop part way can leave a tuple twice, which the next open resolves (see
    ///   [`open_with_budget`](Self::open_with_budget));
    /// - intray's staged drops and claims, discarded ([`SpaceEndpoint::discard_staging`]; the
    ///   open already did, and a re-encoding that stopped part way may leave one);
    /// - a tuple in a SETTLED stage (`error/`, `outbox/`, `.processing/`; never read by the
    ///   layer host, and intray re-encodes only the inbox) is sealed again under its NEW id,
    ///   since a file sealed again in place no longer hashes to its name and intray would take
    ///   it for a corrupt one: [`reseal_settled`].
    ///
    /// Answers how many files it rewrote or removed.
    pub(crate) fn reseal(&self) -> std::result::Result<usize, String> {
        let store = &self.store;
        let sealer = &store.sealer;
        let mut changed = 0;
        for dir in [&store.receipts, &store.staging, &store.leases] {
            for file in read_dir(dir)? {
                if reseal_file(sealer, &file)? {
                    changed += 1;
                }
            }
        }
        let moved = store
            .space
            .reencode(SPACE_NAME)
            .map_err(|e| format!("seal the pending tuples again: {e}"))?;
        {
            let mut index = self.index.write().expect("tuplespace lock");
            for pair in &moved {
                // Intray writes the file with the process's umask (see `drop_tuple`).
                let _ = at_rest::private(&store.tuple_path(&pair.new));
                if let Some(seq) = index.pending_by_cid(&pair.old).map(|meta| meta.seq) {
                    index.recid(seq, pair.new.clone());
                }
            }
        }
        changed += moved.len();
        changed += store
            .space
            .discard_staging(SPACE_NAME)
            .map_err(|e| format!("clear {}'s staging: {e}", store.tuples.display()))?;
        for stage in [ERROR_DIR, OUTBOX_DIR, PROCESSING_DIR] {
            changed += reseal_settled(sealer, &store.tuples.join(stage))?;
        }
        Ok(changed)
    }

    /// Select expired receipts, bounded by a sweep's sequence so replay cannot delete
    /// a later take even if its clock moved backward. Called under the layer write lock.
    pub(crate) fn expired(
        &self,
        before: &str,
        seq: u64,
    ) -> std::result::Result<Vec<String>, String> {
        use std::str::FromStr;
        let cutoff = oxsdatatypes::DateTime::from_str(before)
            .map_err(|_| "the retention cutoff is not an xsd:dateTime".to_string())?;
        let index = self.index.read().expect("tuplespace lock");
        let mut ids = Vec::new();
        for receipt in index.receipts.iter().filter(|r| r.seq < seq) {
            if index.in_play(receipt) {
                continue;
            }
            let taken = oxsdatatypes::DateTime::from_str(&receipt.time)
                .map_err(|_| format!("receipt {} has no valid take time", receipt.iri))?;
            if taken < cutoff {
                ids.push(receipt.id.clone());
            }
        }
        Ok(ids)
    }

    /// The latest take time, in milliseconds since the epoch, among the receipts
    /// [`Self::expired`] selects for `before` and `seq`: the batch's last receipt expired the
    /// millisecond after this plus the window, the instant its `sweep` is recorded at
    /// (ledger #4082). `None` when nothing is expired.
    pub(crate) fn latest_expired(
        &self,
        before: &str,
        seq: u64,
    ) -> std::result::Result<Option<u64>, String> {
        let ids: BTreeSet<String> = self.expired(before, seq)?.into_iter().collect();
        if ids.is_empty() {
            return Ok(None);
        }
        let index = self.index.read().expect("tuplespace lock");
        Ok(index
            .receipts
            .iter()
            .filter(|r| ids.contains(&r.id))
            .map(|r| until_millis(&r.time))
            .max())
    }

    /// Finish a durably logged sweep. Missing files mean a previous attempt deleted them.
    /// Keep the subject reserved, as for a tuple that left the space without a receipt.
    pub(crate) fn expire(&self, before: &str, seq: u64) -> std::result::Result<(), String> {
        let ids = self.expired(before, seq)?;
        if ids.is_empty() {
            return Ok(());
        }
        let mut staged: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for path in read_dir(&self.store.staging)? {
            if let Some(id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".ttl"))
                .and_then(|n| n.split_once('.').map(|(_, id)| id))
            {
                staged.entry(id.to_string()).or_default().push(path);
            }
        }
        let mut deleted = BTreeSet::new();
        let result = (|| {
            for id in ids {
                let path = self.store.receipt_path(&id);
                let mut paths = vec![path, self.store.lease_path(&id)];
                paths.extend(staged.remove(&id).unwrap_or_default());
                // A committed take may still be staged after a failed rename. Delete
                // both copies, so neither a read fallback nor open resurrects it.
                for path in paths {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => {
                            return Err(format!("delete expired receipt {}: {e}", path.display()))
                        }
                    }
                }
                deleted.insert(id);
            }
            Ok(())
        })();
        // Update even on partial failure. Keep take order and reserve expired subjects;
        // a retry uses the logged batch and does not append another aggregate.
        let mut index = self.index.write().expect("tuplespace lock");
        // The receipt each taken subject is current under, by id, before positions move. A
        // subject taken under an expired receipt stays used; a pending one stays pending.
        let current: Vec<(String, String)> = index
            .subjects
            .iter()
            .filter_map(|(subject, place)| match place {
                Place::Taken(i) => Some((subject.clone(), index.receipts[*i].id.clone())),
                _ => None,
            })
            .collect();
        index.receipts.retain(|r| !deleted.contains(&r.id));
        // Never a held lease (it is live), but the index names no receipt that is gone.
        index.held.retain(|(_, id)| !deleted.contains(id));
        index.by_receipt_id.clear();
        let positions: Vec<_> = index
            .receipts
            .iter()
            .enumerate()
            .map(|(i, r)| (i, r.id.clone()))
            .collect();
        for (i, id) in positions {
            index.by_receipt_id.insert(id, i);
        }
        for (subject, id) in current {
            let place = match index.by_receipt_id.get(&id) {
                Some(i) => Place::Taken(*i),
                None => Place::Gone,
            };
            index.subjects.insert(subject, place);
        }
        result
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

    /// Record a leased take's standing, stored first ([`Store::write_lease`]).
    pub fn record_lease(&self, id: &str, lease: Lease) {
        self.index
            .write()
            .expect("tuplespace lock")
            .set_lease(id, lease);
    }

    /// Drop a lapsed take's tuple back into the space, as its receipt holds it (the same
    /// subject and `ik:seq`), and record it pending again. Called under the layer's write lock,
    /// after the lapse is stored.
    pub async fn return_tuple(
        &self,
        layer: &str,
        receipt: &Receipt,
    ) -> std::result::Result<(), String> {
        let tuple = self.store.taken_tuple(receipt)?;
        let cid = self
            .store
            .drop_tuple(rdf::to_turtle(&tuple).as_bytes())
            .await
            .map_err(|e| format!("return <{}> to the space: {e}", receipt.tuple.subject))?;
        let mut meta = tuple_meta(layer, &tuple, &cid)?;
        meta.attempts = receipt.attempt;
        self.index
            .write()
            .expect("tuplespace lock")
            .returned(&receipt.id, meta);
        Ok(())
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
///
/// A staged file that does not read back as a whole receipt is a stage that never completed
/// (a write that failed and whose removal failed too, or a host that stopped mid-write), and
/// no take follows a stage until it is synced whole: it is discarded, whether or not its
/// tuple is pending, rather than committed as a record it never was (ledger #1221).
fn settle_staged(
    store: &Store,
    pending: &BTreeSet<String>,
    layer: &str,
) -> std::result::Result<(), String> {
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
        let bytes = std::fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        // Torn (empty, part of a header, or a body that does not open whole) is a stage that
        // never completed; a wrong key or plaintext is not, and refuses the open instead of
        // discarding a record.
        let opened = match store.sealer.open(&bytes) {
            Ok(opened) => Some(opened),
            Err(Unsealed::Corrupt(_)) => None,
            Err(why) => return Err(why.about(&file)),
        };
        let whole = opened
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .is_some_and(|text| parse_receipt(layer, &text).is_ok());
        if pending.contains(cid) || !whole {
            store.unstage(cid, id);
        } else {
            store.commit(cid, id)?;
        }
    }
    Ok(())
}

/// Seal the file at `path` again, whole, unless it is already sealed to the current identity.
fn reseal_file(sealer: &Sealer, path: &Path) -> std::result::Result<bool, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    if sealer.is_current(&bytes) {
        return Ok(false);
    }
    let opened = sealer.open(&bytes).map_err(|why| why.about(path))?;
    at_rest::replace(path, &sealer.seal(&opened)?)?;
    Ok(true)
}

/// Seal again every file of a settled stage of the space (`dir`, absent for a space no reactor
/// ever ran over) not sealed to the current identity, and answer how many changed.
///
/// A tuple (`{id}.tuple`) is named by the hash of its stored bytes, so it is sealed again under
/// its NEW id: the new file written whole, then its notes (`{id}.err`, `{id}.out`) renamed to
/// the new id, then the old file removed. A stop between leaves the tuple twice, which is a
/// dead letter or a settled answer seen twice, never a tuple lost. A file that does not hash to
/// its name is one intray already holds as corrupt: it is sealed again in place, keeping its
/// name and so its standing. Then each `.out` note (what a handler answered, so possibly tuple
/// content) is sealed in place; an `.err` note, intray's sentence or a handler's error, is left
/// as it is, as before.
fn reseal_settled(sealer: &Sealer, dir: &Path) -> std::result::Result<usize, String> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut changed = 0;
    for file in read_dir(dir)? {
        let Some(id) = file
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".tuple"))
            .map(str::to_string)
        else {
            continue;
        };
        let bytes = std::fs::read(&file).map_err(|e| format!("read {}: {e}", file.display()))?;
        if sealer.is_current(&bytes) {
            continue;
        }
        let opened = sealer.open(&bytes).map_err(|why| why.about(&file))?;
        let sealed = sealer.seal(&opened)?;
        changed += 1;
        if tuple_id(&bytes) != id {
            at_rest::replace(&file, &sealed)?;
            continue;
        }
        let new = tuple_id(&sealed);
        at_rest::replace(&dir.join(format!("{new}.tuple")), &sealed)?;
        for note in ["err", "out"] {
            let from = dir.join(format!("{id}.{note}"));
            if from.is_file() {
                let to = dir.join(format!("{new}.{note}"));
                std::fs::rename(&from, &to)
                    .map_err(|e| format!("rename {} to {}: {e}", from.display(), to.display()))?;
            }
        }
        std::fs::remove_file(&file).map_err(|e| format!("remove {}: {e}", file.display()))?;
    }
    for file in read_dir(dir)? {
        if file.extension().is_some_and(|x| x == "out") && reseal_file(sealer, &file)? {
            changed += 1;
        }
    }
    Ok(changed)
}

/// The `out` event a stored tuple carries.
pub fn out_event(meta: &TupleMeta) -> Event {
    Event {
        sweep: None,
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
        sweep: None,
        seq: receipt.seq,
        time: receipt.time.clone(),
        kind: "taken".into(),
        iri: receipt.tuple.subject.clone(),
        by: receipt.taker.clone(),
    }
}

/// The `returned` event of a lapsed take, by the host's lease sweep.
pub fn returned_event(receipt: &Receipt) -> Option<Event> {
    match &receipt.lease {
        Some(Lease {
            state: LeaseState::Lapsed { seq, time },
            ..
        }) => Some(Event {
            sweep: None,
            seq: *seq,
            time: time.clone(),
            kind: "returned".into(),
            iri: receipt.tuple.subject.clone(),
            by: LEASE_AGENT.into(),
        }),
        _ => None,
    }
}

/// Who a `returned` event is by: the host's lease sweep, as `urn:layers:retention` is its
/// retention sweep.
pub const LEASE_AGENT: &str = "urn:layers:lease";

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
        attempts: 0,
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
    let (mut attempt, mut until) = (None, None);
    let (seq_p, attempt_p, until_p) = (
        rdf::ik_seq(),
        rdf::layers_attempt(),
        rdf::layers_lease_until(),
    );
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
        } else if p == attempt_p {
            if let Term::Literal(l) = &t.object {
                attempt = l.value().parse::<u64>().ok();
            }
        } else if p == until_p {
            if let Term::Literal(l) = &t.object {
                until = Some(l.value().to_string());
            }
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
        attempt: attempt.unwrap_or(1),
        lease: until.map(|until| Lease {
            until,
            state: LeaseState::Held,
        }),
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
/// Refused (as a bad `content`): more than the layer's `entry_max` bytes; JSON-LD nested
/// deeper than [`crate::channel::MAX_JSON_DEPTH`]; a parse error; a named graph; anything
/// but exactly one subject, an IRI under `urn:layer:{layer}:` that is
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
            attempts: 0,
            digest: self.digest,
        }
    }
}

/// A receipt's document: the take (a `prov:Activity` that used the tuple, associated with
/// its taker, ended at `time`, numbered `seq` in the feed), then the tuple as taken.
///
/// A leased take (`lease_until`) records when its lease lapses, and a take that is not the
/// tuple's first (`attempt` above 1) or is leased records which attempt it is.
#[allow(clippy::too_many_arguments)]
pub fn receipt_document(
    layer: &str,
    id: &str,
    taker: &str,
    time: &str,
    seq: u64,
    subject: &str,
    tuple: &[Triple],
    attempt: u64,
    lease_until: Option<&str>,
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
    if attempt > 1 || lease_until.is_some() {
        triples.push(Triple::new(
            node(&iri),
            node(&rdf::layers_attempt()),
            Literal::new_typed_literal(attempt.to_string(), node(rdf::XSD_INTEGER)),
        ));
    }
    if let Some(until) = lease_until {
        triples.push(Triple::new(
            node(&iri),
            node(&rdf::layers_lease_until()),
            Literal::new_typed_literal(until, node(rdf::XSD_DATE_TIME)),
        ));
    }
    triples.extend(tuple.iter().cloned());
    (iri, rdf::to_turtle(&triples))
}

/// Build the [`Receipt`] for a take whose document is `text`.
pub fn receipt_of(layer: &str, text: &str) -> Result<Receipt> {
    parse_receipt(layer, text).map_err(Error::Endpoint)
}
