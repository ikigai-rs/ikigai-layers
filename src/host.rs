//! The host: the configuration it was opened with, and each layer's state.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::channel::Channel;
use crate::config::{self, Config, ConfigError, LayerDef, Model, Participants};
use crate::feed::{self, Event, FeedLog, Notify};
use crate::tuplespace::Tuplespace;
use ikigai_store::budget::EvaluationBudget;

/// One layer's state.
pub struct LayerState {
    /// Its definition, fixed for the life of the host.
    pub def: LayerDef,
    /// Serializes the layer's writes, ACROSS the admission sub-request (an await), so a
    /// sequence number is assigned, validated, stored and logged as one step.
    pub(crate) write: futures::lock::Mutex<()>,
    /// Its feed.
    pub(crate) feed: Mutex<FeedLog>,
    /// The model's state.
    pub(crate) model: ModelState,
}

/// The state each access model holds. One arm per built model; the `graph` model
/// (ledger #1170, arc 14) would be the next.
pub enum ModelState {
    /// A channel's entries.
    Channel(RwLock<Channel>),
    /// A tuplespace's space, receipts and index.
    Tuplespace(Tuplespace),
}

impl LayerState {
    /// The channel, for a channel layer.
    ///
    /// # Panics
    /// On a layer of another model: a channel's actions are bound only on channel layers,
    /// so this is a bug in this crate, never a caller's input.
    pub(crate) fn channel(&self) -> &RwLock<Channel> {
        match &self.model {
            ModelState::Channel(channel) => channel,
            _ => unreachable!("channel actions are bound only on channel layers"),
        }
    }

    /// The tuplespace, for a tuplespace layer.
    ///
    /// # Panics
    /// On a layer of another model, as [`channel`](Self::channel).
    pub(crate) fn tuplespace(&self) -> &Tuplespace {
        match &self.model {
            ModelState::Tuplespace(space) => space,
            _ => unreachable!("tuplespace actions are bound only on tuplespace layers"),
        }
    }

    /// The feed's events after `after`, at most [`feed::MAX_EVENTS`].
    pub fn events_after(&self, after: u64) -> Vec<Event> {
        self.feed.lock().expect("feed lock").after(after).to_vec()
    }

    /// The last sequence number in the feed.
    pub fn last_seq(&self) -> u64 {
        self.feed.lock().expect("feed lock").last()
    }
}

/// The file in a state directory that its one host holds locked: [`Host::open`] takes it,
/// and the host's drop (or its process's exit, however it ends) releases it. It holds the
/// holder's process id, for the refusal a second opener reads.
pub const LOCK_FILE: &str = "host.lock";

/// A layer host: what [`crate::space`] binds.
///
/// Opened over two directories: the CONFIG directory (definitions, shapes, contexts,
/// participants, manifests: the operator's, read-only here) and the STATE directory (each
/// layer's entries and feed, `<state>/{layer}/`, written only by this host). **One host per
/// state directory**: the write lock that keeps a feed gapless is in-process, so the host
/// holds [`LOCK_FILE`] in the state directory exclusively for as long as it lives, and a
/// second host (another door, or another `Host::open` in the same process) is refused
/// naming the holder's pid (ledger #1267).
///
/// Definitions are read once, at open, because each layer binds its own endpoints; adding
/// or changing a layer means opening the host again. Participants and manifests are read
/// on every use, because the door's enrollment is live (cli re-reads `clients.json` per
/// connection) and a participant enrolled a minute ago must be able to write.
pub struct Host {
    config_dir: PathBuf,
    layers: BTreeMap<String, Arc<LayerState>>,
    notify: Arc<Notify>,
    /// The ONE time budget every tuplespace layer's `match=` runs within (ledger #1362).
    match_budget: EvaluationBudget,
    /// Held, never read: the state directory's lock lives exactly as long as the host.
    _lock: File,
}

impl Host {
    /// Open a host: check the whole configuration (a manifest that breaks the isolation
    /// rule stops the host here, not at its first read, and so do shapes the validator cannot
    /// compile), take the state directory's lock (refused, naming the holder's pid, when
    /// another host holds it), then open every layer's state, repairing a feed that lost the
    /// event of an entry stored just before a crash.
    pub fn open(
        config_dir: impl AsRef<Path>,
        state_dir: impl AsRef<Path>,
    ) -> Result<Arc<Host>, ConfigError> {
        let config = Config::load(&config_dir)?;
        let match_budget = EvaluationBudget::new(config::load_match_budget(&config.dir)?);
        for (name, def) in &config.layers {
            compile_shapes(def).map_err(|e| ConfigError(format!("layer `{name}`: {e}")))?;
        }
        let state_dir = state_dir.as_ref();
        let lock = lock_state(state_dir).map_err(ConfigError)?;
        let mut layers = BTreeMap::new();
        for (name, def) in config.layers {
            let dir = state_dir.join(&name);
            // A CLONE: every layer shares the one overdue count (ledger #1362).
            let state = open_layer(def, &dir, match_budget.clone())
                .map_err(|e| ConfigError(format!("layer `{name}` state: {e}")))?;
            layers.insert(name, Arc::new(state));
        }
        Ok(Arc::new(Host {
            config_dir: config.dir,
            layers,
            notify: Arc::new(Notify::default()),
            match_budget,
            _lock: lock,
        }))
    }

    /// The layers, by name.
    pub fn layers(&self) -> &BTreeMap<String, Arc<LayerState>> {
        &self.layers
    }

    /// One layer.
    pub fn layer(&self, name: &str) -> Option<&Arc<LayerState>> {
        self.layers.get(name)
    }

    /// The time budget every tuplespace layer's `rd match=` runs within: ONE per host, from
    /// [`config::load_match_budget`], cloned into each layer, so its
    /// [`overdue_evaluations`](EvaluationBudget::overdue_evaluations) counts the templates
    /// still running after their callers were answered across every layer, and its
    /// `max_overdue` refuses the next one, in any layer, while that many are (ledger #1362).
    pub fn match_budget(&self) -> &EvaluationBudget {
        &self.match_budget
    }

    /// The change signal every feed append bumps.
    pub fn notify(&self) -> &Notify {
        &self.notify
    }

    /// The participants graph, read now.
    pub fn participants(&self) -> Result<Participants, ConfigError> {
        config::load_participants(&self.config_dir)
    }

    /// The manifests, read now and checked against this host's layers.
    pub fn collabs(&self) -> Result<BTreeMap<String, config::Manifest>, ConfigError> {
        self.collabs_with(&self.participants()?)
    }

    /// The manifests, read now and checked against this host's layers and `participants`
    /// (one reading of the participants graph, for a caller that also serves it).
    pub(crate) fn collabs_with(
        &self,
        participants: &Participants,
    ) -> Result<BTreeMap<String, config::Manifest>, ConfigError> {
        let defs: BTreeMap<String, LayerDef> = self
            .layers
            .iter()
            .map(|(name, state)| (name.clone(), state.def.clone()))
            .collect();
        config::load_collabs(&self.config_dir, &defs, participants)
    }
}

/// Compile a layer's shapes the way admission will use them, by validating an empty graph
/// against them: shapes the validator cannot compile would admit nothing, so they stop the
/// host at open rather than refusing every write later.
fn compile_shapes(def: &LayerDef) -> Result<(), String> {
    ikigai_shacl::validate_report("", &def.shapes)
        .map(|_| ())
        .map_err(|e| format!("the validator cannot compile its shapes ({e})"))
}

/// Take `<state>/`[`LOCK_FILE`] exclusively, creating the directory and the file as needed,
/// and write this process's id into it. The lock is advisory and released by the operating
/// system when the file is closed, so a crashed host never leaves a directory locked: the
/// file it leaves behind is only a stale pid, overwritten by the next host.
fn lock_state(state_dir: &Path) -> Result<File, String> {
    std::fs::create_dir_all(state_dir)
        .map_err(|e| format!("create {}: {e}", state_dir.display()))?;
    let path = state_dir.join(LOCK_FILE);
    // Never truncated on open: until the lock is ours, the pid in it is the holder's.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => {
            let mut held = String::new();
            let holder = match file.read_to_string(&mut held) {
                Ok(_) if !held.trim().is_empty() => format!("pid {}", held.trim()),
                // The holder has not written its pid yet, or the platform's lock is
                // mandatory and refuses the read.
                _ => "a pid it has not written".to_string(),
            };
            return Err(format!(
                "state directory {} is held by another layer host ({holder}, {}): one host per \
                 state directory, since the lock that keeps each feed gapless is in-process",
                state_dir.display(),
                path.display()
            ));
        }
        Err(TryLockError::Error(e)) => {
            return Err(format!("lock {}: {e}", path.display()));
        }
    }
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.flush())
        .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(file)
}

fn open_layer(def: LayerDef, dir: &Path, budget: EvaluationBudget) -> Result<LayerState, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let feed_path = dir.join("feed.log");
    feed::touch(&feed_path)?;
    let mut feed = FeedLog::open(&feed_path)?;
    let model = match def.model {
        Model::Channel => {
            let channel = Channel::open(&def, dir)?;
            // The entry file is written before its feed event, so a crash between the two
            // leaves entries the feed does not list. They were never acknowledged, but
            // they are stored and numbered: log them now, in order, so the feed stays the
            // complete, gapless record of the layer.
            let logged = feed.last();
            for meta in channel.entries().iter().filter(|m| m.seq > logged) {
                if meta.seq != feed.last() + 1 {
                    return Err(format!(
                        "entry {} has sequence {} but the feed ends at {}: a gap no repair \
                         can close",
                        meta.iri,
                        meta.seq,
                        feed.last()
                    ));
                }
                feed.append(Event {
                    seq: meta.seq,
                    time: meta.created.clone(),
                    kind: "say".into(),
                    iri: meta.iri.clone(),
                    by: meta.author.clone(),
                })?;
            }
            ModelState::Channel(RwLock::new(channel))
        }
        Model::Tuplespace => {
            let logged: Vec<String> = feed
                .events()
                .iter()
                .filter(|e| e.kind == "out")
                .map(|e| e.iri.clone())
                .collect();
            let (space, stored) = Tuplespace::open_with_budget(&def, dir, &logged, budget)?;
            // A drop or a take is stored before its feed event (a tuple is in the space, a
            // receipt is committed), so a crash between the two leaves events the feed has
            // not logged: log them now, in order, as the channel does.
            let logged_to = feed.last();
            for event in stored.into_iter().filter(|e| e.seq > logged_to) {
                if event.seq != feed.last() + 1 {
                    return Err(format!(
                        "{} `{}` has sequence {} but the feed ends at {}: a gap no repair \
                         can close",
                        event.kind,
                        event.iri,
                        event.seq,
                        feed.last()
                    ));
                }
                feed.append(event)?;
            }
            ModelState::Tuplespace(space)
        }
    };
    Ok(LayerState {
        def,
        write: futures::lock::Mutex::new(()),
        feed: Mutex::new(feed),
        model,
    })
}
