//! A layer's feed: an append-only log with a GAPLESS sequence, and the waiting that lets a
//! follower park until something happens instead of polling.
//!
//! One file per layer, `<state>/{layer}/feed.log`, one event per line:
//!
//! ```text
//! {seq}\t{time}\t{kind}\t{iri}\t{by}
//! ```
//!
//! `seq` starts at 1 and every event is the previous one plus one, assigned under the
//! layer's write lock by the verb that wrote it, so a follower that resumes after the last
//! number it saw cannot miss an event, and a gap is a defect rather than a quiet hour.
//! Lines are written whole and synced; a torn last line (a crash mid-write) is dropped on
//! the next open, because the event it began was never acknowledged.
//!
//! **A sequence number is spent once a write that carries it is stored**, whether or not its
//! event reached the log (ledger #1218). A write stores first and logs second, so an append
//! that fails leaves an event the log OWES: [`FeedLog::owe`] keeps it, the next number handed
//! out ([`FeedLog::next`]) is past it, and [`FeedLog::settle`] logs it, in order, before any
//! later event. A layer whose log cannot be written takes no further writes until it can,
//! and a host that stops first logs the stored write when it next opens, from the record.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use serde::Serialize;

/// The longest `wait=` a feed read may ask for, in seconds.
pub const MAX_WAIT_SECS: u64 = 60;

/// The most events one feed answer carries. A follower reads again after the last
/// sequence number it received; the sequence is gapless, so paging loses nothing.
pub const MAX_EVENTS: usize = 1000;

/// One event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Event {
    /// Its place in the layer's feed, from 1, gapless.
    pub seq: u64,
    /// When, as `xsd:dateTime` (the kernel clock).
    pub time: String,
    /// What happened: `say` (the channel model), `out` or `taken` (the tuplespace model).
    pub kind: String,
    /// What it happened to: an entry's IRI for `say`, a tuple's subject for `out` and
    /// `taken`.
    pub iri: String,
    /// Who did it: the principal.
    pub by: String,
}

impl Event {
    fn line(&self) -> String {
        format!(
            "{}\t{}\t{}\t{}\t{}\n",
            self.seq, self.time, self.kind, self.iri, self.by
        )
    }

    fn parse(line: &str) -> Option<Event> {
        let mut parts = line.split('\t');
        let event = Event {
            seq: parts.next()?.parse().ok()?,
            time: parts.next()?.to_string(),
            kind: parts.next()?.to_string(),
            iri: parts.next()?.to_string(),
            by: parts.next()?.to_string(),
        };
        parts.next().is_none().then_some(event)
    }
}

/// One layer's feed, held open for appending.
pub struct FeedLog {
    path: PathBuf,
    events: Vec<Event>,
    /// The bytes of the whole, logged events: where the next line goes, whatever a failed
    /// append left after it.
    len: u64,
    /// Events whose writes are stored but whose lines are not logged yet, in order.
    owed: Vec<Event>,
}

impl FeedLog {
    /// Open (or create) the log at `path`, checking that it is gapless from 1.
    pub fn open(path: &Path) -> Result<FeedLog, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        // A torn last line was never acknowledged: drop it, so the next append starts on
        // a line of its own.
        let whole = match text.rfind('\n') {
            Some(end) => &text[..=end],
            None => "",
        };
        if whole.len() != text.len() {
            let file = OpenOptions::new()
                .write(true)
                .open(path)
                .map_err(|e| format!("open {}: {e}", path.display()))?;
            file.set_len(whole.len() as u64)
                .and_then(|()| file.sync_all())
                .map_err(|e| format!("truncate the torn tail of {}: {e}", path.display()))?;
        }
        let mut events = Vec::new();
        for (n, line) in whole.lines().enumerate() {
            let event = Event::parse(line)
                .ok_or_else(|| format!("{}:{}: not a feed event", path.display(), n + 1))?;
            if event.seq != n as u64 + 1 {
                return Err(format!(
                    "{}:{}: sequence {} where {} was due: the feed is not gapless",
                    path.display(),
                    n + 1,
                    event.seq,
                    n + 1
                ));
            }
            events.push(event);
        }
        Ok(FeedLog {
            path: path.to_path_buf(),
            events,
            len: whole.len() as u64,
            owed: Vec::new(),
        })
    }

    /// The last sequence number logged (0 for an empty feed).
    pub fn last(&self) -> u64 {
        self.events.len() as u64
    }

    /// The sequence number the next write carries: past every logged event AND every owed
    /// one, so a number a stored write holds is never handed out twice.
    pub fn next(&self) -> u64 {
        self.last() + self.owed.len() as u64 + 1
    }

    /// Whether events are owed (stored writes the log does not list yet).
    pub fn owes(&self) -> bool {
        !self.owed.is_empty()
    }

    /// The first owed event: the stored write the log must list before anything later.
    pub fn first_owed(&self) -> Option<&Event> {
        self.owed.first()
    }

    /// Append the event numbered `last() + 1`, synced before it is acknowledged. The line
    /// goes after the last whole event, over anything a failed append left behind.
    ///
    /// On an error nothing is logged: the caller undoes its write, or [`owe`](Self::owe)s
    /// the event.
    ///
    /// # Panics
    /// If an event is owed (it must be [`settle`](Self::settle)d first), or if `event.seq`
    /// is not the next number: the caller took it under the layer's write lock from
    /// [`next`](Self::next), so either is a bug in this crate.
    pub fn append(&mut self, event: Event) -> Result<(), String> {
        assert!(self.owed.is_empty(), "an owed event is logged first");
        self.write(event)
    }

    fn write(&mut self, event: Event) -> Result<(), String> {
        assert_eq!(event.seq, self.last() + 1, "a feed is gapless");
        let line = event.line();
        let written = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&self.path)
            .and_then(|mut file| {
                file.set_len(self.len)?;
                file.seek(SeekFrom::Start(self.len))?;
                file.write_all(line.as_bytes())?;
                file.sync_data()
            });
        if let Err(e) = written {
            // Best effort: leave no part of the line that was never acknowledged. The next
            // append truncates to the same length anyway.
            if let Ok(file) = OpenOptions::new().write(true).open(&self.path) {
                let _ = file.set_len(self.len);
            }
            return Err(format!("append to {}: {e}", self.path.display()));
        }
        self.len += line.len() as u64;
        self.events.push(event);
        Ok(())
    }

    /// Keep an event whose write is stored but whose line could not be logged: its number
    /// is spent, and [`settle`](Self::settle) logs it before anything later.
    ///
    /// # Panics
    /// If `event.seq` is not [`next`](Self::next): a bug in this crate.
    pub fn owe(&mut self, event: Event) {
        assert_eq!(event.seq, self.next(), "a feed is gapless");
        self.owed.push(event);
    }

    /// Log every owed event, in order. Answers how many it logged; on an error the rest
    /// stay owed.
    pub fn settle(&mut self) -> Result<usize, String> {
        let mut logged = 0;
        while !self.owed.is_empty() {
            let event = self.owed[0].clone();
            self.write(event)?;
            self.owed.remove(0);
            logged += 1;
        }
        Ok(logged)
    }

    /// Every event, in order.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The events after `after`, at most [`MAX_EVENTS`].
    pub fn after(&self, after: u64) -> &[Event] {
        let start = (after as usize).min(self.events.len());
        let end = (start + MAX_EVENTS).min(self.events.len());
        &self.events[start..end]
    }
}

/// Make sure a feed file exists (so a layer's directory says it is a layer).
pub(crate) fn touch(path: &Path) -> Result<(), String> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map(|_: File| ())
        .map_err(|e| format!("create {}: {e}", path.display()))
}

/// A cursor over several layers' feeds, `talk:12,tasks-bobby:40`: the last sequence number
/// the follower saw in each.
///
/// ```
/// use ikigai_layers::feed::{format_cursor, parse_cursor};
///
/// let cursor = parse_cursor("talk:12, tasks-bobby:40").unwrap();
/// assert_eq!(cursor["talk"], 12);
/// assert_eq!(format_cursor(&cursor), "talk:12,tasks-bobby:40");
/// assert!(parse_cursor("talk").is_err());
/// assert!(parse_cursor("talk:1,talk:2").is_err());
/// ```
pub fn parse_cursor(text: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut cursor = BTreeMap::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (layer, seq) = part
            .rsplit_once(':')
            .ok_or_else(|| format!("`{part}` is not `layer:seq`"))?;
        let seq: u64 = seq
            .parse()
            .map_err(|_| format!("`{part}`: `{seq}` is not a sequence number"))?;
        if cursor.insert(layer.to_string(), seq).is_some() {
            return Err(format!("layer `{layer}` is named twice"));
        }
    }
    if cursor.is_empty() {
        return Err("names no layer (`layer:seq,…`)".into());
    }
    Ok(cursor)
}

/// The cursor's text form, in layer order.
pub fn format_cursor(cursor: &BTreeMap<String, u64>) -> String {
    cursor
        .iter()
        .map(|(layer, seq)| format!("{layer}:{seq}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// The host's change signal: a generation every feed append bumps, and the wakers of every
/// read parked on it. One for the whole host, so one wait can cover any set of layers;
/// a woken reader re-checks its own layers, and a wake for another layer is a spurious
/// wake, which costs one check.
#[derive(Default)]
pub struct Notify {
    state: Mutex<(u64, Vec<Waker>)>,
}

impl Notify {
    /// The current generation.
    pub fn generation(&self) -> u64 {
        self.state.lock().expect("notify lock").0
    }

    /// Bump the generation and wake every parked reader.
    pub fn bump(&self) {
        let wakers = {
            let mut state = self.state.lock().expect("notify lock");
            state.0 += 1;
            std::mem::take(&mut state.1)
        };
        for waker in wakers {
            waker.wake();
        }
    }

    /// A future that is ready once the generation differs from `seen`.
    pub fn changed(&self, seen: u64) -> Changed<'_> {
        Changed { notify: self, seen }
    }
}

/// See [`Notify::changed`].
pub struct Changed<'a> {
    notify: &'a Notify,
    seen: u64,
}

impl std::future::Future for Changed<'_> {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut state = self.notify.state.lock().expect("notify lock");
        if state.0 != self.seen {
            return Poll::Ready(());
        }
        if !state.1.iter().any(|w| w.will_wake(cx.waker())) {
            state.1.push(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// Park until `ready()` says yes or `wait` has passed, whichever is first. `ready` is
/// checked first, so a zero wait is a plain read.
pub async fn wait_until(notify: &Notify, wait: Duration, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + wait;
    loop {
        // Read the generation BEFORE checking, so an append between the check and the
        // park is seen as a change rather than slept through.
        let seen = notify.generation();
        if ready() {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        let changed = notify.changed(seen);
        let timer = futures_timer::Delay::new(deadline - now);
        futures::pin_mut!(changed);
        futures::future::select(changed, timer).await;
    }
}
