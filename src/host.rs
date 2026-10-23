//! The host: the configuration it was opened with, and each layer's state.

use std::collections::BTreeMap;
use std::fs::{File, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::at_rest::{self, KeyCheck, Migration, Sealer, StateKey};
use crate::channel::Channel;
use crate::config::{self, Config, ConfigError, LayerDef, Model, Participants};
use crate::feed::{self, Event, FeedLog, Notify};
use crate::json::{egress_context, Egress};
use crate::onboard::apply::{LobbyRate, Onboard};
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
    /// The context its JSON-LD faces compact with: its egress context with the host's stamp
    /// terms merged in ([`crate::json::egress_context`]), checked when the host opened.
    pub(crate) egress: serde_json::Value,
}

/// The state each access model holds. One arm per built model; the `graph` model
/// (ledger #1170, arc 14) would be the next.
// One per layer, held behind the layer's `Arc` for the host's life: the arms' sizes (the
// tuplespace's grew with its sealer and budget) cost nothing worth a box.
#[allow(clippy::large_enum_variant)]
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
/// **State is sealed at rest** ([`crate::at_rest`], ledger #2011): every file the host writes
/// under `<state>/{layer}/` is age ciphertext sealed to the host's key, a [`StateKey`] read
/// from an identity file whose PATH the deployment hands over (config home and flags, never
/// an environment variable). Files are created `0600` and directories `0700`, and an open makes
/// every file and directory already there the same.
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
    /// The host's own JSON-LD processor, bound into no space (ledger #1371).
    json: Egress,
    /// Onboarding (ledger #2009): the `[onboard]` table, when `layers.toml` has one.
    onboard: Option<Onboard>,
    /// Every lobby's rate of says.
    lobby_rate: LobbyRate,
    /// Held, never read: the state directory's lock lives exactly as long as the host.
    _lock: File,
}

impl Host {
    /// Open a host whose state is sealed with `key`: [`open_with`](Self::open_with) and
    /// [`Migration::None`], so a state directory holding any plaintext file is refused, naming
    /// it and the migration option.
    pub fn open(
        config_dir: impl AsRef<Path>,
        state_dir: impl AsRef<Path>,
        key: &StateKey,
    ) -> Result<Arc<Host>, ConfigError> {
        Self::open_with(config_dir, state_dir, key, Migration::None)
    }

    /// Open a host: check the whole configuration (a manifest that breaks the isolation
    /// rule stops the host here, not at its first read, and so do shapes the validator cannot
    /// compile, and so does an egress context that cannot carry the host's stamps to its JSON-LD
    /// faces, see [`Egress::check`]), take the state directory's lock (refused, naming the
    /// holder's pid, when another host holds it), make its files `0600` and directories
    /// `0700`, check `key` against the directory's [`at_rest::KEY_CHECK`] (a key that does
    /// not open it is refused before anything else is read), then open every layer's state,
    /// repairing a feed that lost the event of an entry stored just before a crash.
    ///
    /// With [`Migration::None`] a layer directory holding any plaintext file refuses the
    /// open, naming the file. With [`Migration::Reseal`] each layer is first opened accepting
    /// plaintext, everything in it not sealed to the current identity is sealed again in
    /// place, and it is then opened as [`Migration::None`] would open it, so the open fails if
    /// anything was left behind.
    ///
    /// Only the layers the configuration defines are the host's state: a directory under the
    /// state directory that no definition names is neither read nor sealed.
    pub fn open_with(
        config_dir: impl AsRef<Path>,
        state_dir: impl AsRef<Path>,
        key: &StateKey,
        migration: Migration,
    ) -> Result<Arc<Host>, ConfigError> {
        let config = Config::load(&config_dir)?;
        let match_budget = EvaluationBudget::new(config::load_match_budget(&config.dir)?);
        let onboard = config::load_onboard(&config.dir, &config.layers)?;
        if let Some(onboard) = &onboard {
            config::check_controller_says(onboard, &config.collabs)?;
        }
        let onboard = onboard.map(Onboard::new);
        let json = Egress::new();
        let mut egress = BTreeMap::new();
        for (name, def) in &config.layers {
            compile_shapes(def).map_err(|e| ConfigError(format!("layer `{name}`: {e}")))?;
            let context = egress_context(&def.context)
                .and_then(|context| json.check(name, &context).map(|()| context))
                .map_err(|e| ConfigError(format!("layer `{name}`: {e}")))?;
            egress.insert(name.clone(), context);
        }
        let state_dir = state_dir.as_ref();
        let lock = lock_state(state_dir).map_err(ConfigError)?;
        at_rest::tighten(state_dir).map_err(ConfigError)?;
        let check = KeyCheck::verify(state_dir, key).map_err(ConfigError)?;
        if migration == Migration::None {
            refuse_plaintext(state_dir, config.layers.keys()).map_err(ConfigError)?;
        }
        let sealer = Sealer::new(key);
        let mut layers = BTreeMap::new();
        for (name, def) in config.layers {
            let dir = state_dir.join(&name);
            let context = egress.remove(&name).expect("checked above");
            if migration == Migration::Reseal {
                // A CLONE: every layer shares the one overdue count (ledger #1362).
                open_layer(
                    def.clone(),
                    &dir,
                    match_budget.clone(),
                    context.clone(),
                    &Sealer::migrating(key),
                )
                .and_then(|state| reseal_layer(&state))
                .map_err(|e| ConfigError(format!("layer `{name}` state, sealing it: {e}")))?;
            }
            let state = open_layer(def, &dir, match_budget.clone(), context, &sealer)
                .map_err(|e| ConfigError(format!("layer `{name}` state: {e}")))?;
            layers.insert(name, Arc::new(state));
        }
        check
            .settle(key, migration == Migration::Reseal)
            .map_err(ConfigError)?;
        let host = Arc::new(Host {
            config_dir: config.dir,
            layers,
            notify: Arc::new(Notify::default()),
            match_budget,
            json,
            onboard,
            lobby_rate: LobbyRate::default(),
            _lock: lock,
        });
        // Catch up on the grants layer: an entitlement said while no host ran is applied now,
        // and one already applied changes nothing (ledger #2009).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        futures::executor::block_on(host.apply_entitlements(ikigai_core::Time::from_millis(now)));
        Ok(host)
    }

    /// The configuration directory the host was opened over: where its door's enrollment
    /// lives, and where applying an entitlement writes.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// The lobbies (`lobby = true` channels), by name.
    pub fn lobbies(&self) -> Vec<&str> {
        self.layers
            .iter()
            .filter(|(_, s)| s.def.lobby)
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// The capability a door mints for a certificate its enrollment does not name, on this
    /// host: `say` on every lobby and nothing else, under
    /// [`crate::onboard::lobby_principal`] (`host` is the door's instance name). `Ok(None)`
    /// when the host has no lobby: the door then refuses the certificate, as it always did.
    ///
    /// With ikigai-cli at or after the release carrying
    /// [f1d2124](https://github.com/ikigai-rs/ikigai-cli/commit/f1d2124b6eff40512aed346fd2a8ffa3ef917771),
    /// a QUIC door with a lobby admits an unlisted, in-window client certificate and mints
    /// this capability: `say` on the lobbies only. A door without a lobby still refuses it.
    /// Set `[onboard]`'s `trust_dir` to the door's `<quic cert dir>/clients`, so an applied
    /// entitlement installs the certificate where the door reads it. Start the door with a
    /// short instance name, e.g. `--name lobby`, for the lobby principal (ledger #2009).
    pub fn lobby_capability(
        &self,
        host: &str,
        fingerprint: &str,
    ) -> Result<Option<ikigai_core::Capability>, String> {
        let lobbies = self.lobbies();
        if lobbies.is_empty() {
            return Ok(None);
        }
        let principal = crate::onboard::lobby_principal(host, fingerprint)?;
        ikigai_core::Capability::scoped(
            lobbies
                .iter()
                .map(|layer| crate::name::layer_cap("say", layer)),
        )
        .with_principal(&principal)
        .map(Some)
        .map_err(|e| format!("{principal}: {e}"))
    }

    /// The onboarding status ([`crate::onboard::STATUS`]), when the host has `[onboard]`.
    pub fn onboard_status(&self) -> Option<crate::onboard::Status> {
        self.onboard.as_ref().map(Onboard::status)
    }

    /// The onboarding configuration, when the host has one.
    pub fn onboard_config(&self) -> Option<&config::OnboardConfig> {
        self.onboard.as_ref().map(|o| &o.config)
    }

    /// Apply the grants layer's entitlements, at `now`. Nothing to do without `[onboard]`.
    pub(crate) async fn apply_entitlements(&self, now: ikigai_core::Time) {
        if let Some(onboard) = &self.onboard {
            onboard.apply(self, now).await;
        }
    }

    pub(crate) fn lobby_rate(&self) -> &LobbyRate {
        &self.lobby_rate
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

    /// The host's own JSON-LD processor, which every layer's JSON-LD faces compact through.
    pub(crate) fn json(&self) -> &Egress {
        &self.json
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
    at_rest::private_dir(state_dir)?;
    let path = state_dir.join(LOCK_FILE);
    // Never truncated on open: until the lock is ours, the pid in it is the holder's.
    let mut file = at_rest::private_file()
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

/// Refuse a state directory whose layers hold any plaintext file, naming one: state written
/// before it was sealed at rest is sealed by one open with [`Migration::Reseal`], never read as
/// if it were. Intray's `.err` notes (an id and a fixed sentence, written by intray itself when
/// it quarantines a tuple) are not layer state.
fn refuse_plaintext<'a>(
    state_dir: &Path,
    layers: impl Iterator<Item = &'a String>,
) -> Result<(), String> {
    let mut found = Vec::new();
    for name in layers {
        let mut dirs = vec![state_dir.join(name)];
        while let Some(dir) = dirs.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("read {}: {e}", dir.display())),
            };
            for entry in entries {
                let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
                let path = entry.path();
                let kind = entry
                    .file_type()
                    .map_err(|e| format!("stat {}: {e}", path.display()))?;
                if kind.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if !kind.is_file() || path.extension().is_some_and(|x| x == "err") {
                    continue;
                }
                let plaintext = if path.file_name().is_some_and(|n| n == "feed.log") {
                    let bytes = std::fs::read(&path)
                        .map_err(|e| format!("read {}: {e}", path.display()))?;
                    at_rest::feed_is_plaintext(&String::from_utf8_lossy(&bytes))
                } else {
                    let mut head = Vec::with_capacity(at_rest::MAGIC.len());
                    File::open(&path)
                        .and_then(|file| {
                            file.take(at_rest::MAGIC.len() as u64)
                                .read_to_end(&mut head)
                        })
                        .map_err(|e| format!("read {}: {e}", path.display()))?;
                    at_rest::is_plaintext(&head)
                };
                if plaintext {
                    found.push(path);
                }
            }
        }
    }
    match found.first() {
        None => Ok(()),
        Some(first) => Err(format!(
            "state directory {} holds {} plaintext file(s), {} among them: layer state is sealed \
             at rest (ledger #2011), so open the host once with the migration option \
             (`Migration::Reseal`) to seal them in place",
            state_dir.display(),
            found.len(),
            first.display()
        )),
    }
}

/// Seal again everything in a layer's state not sealed to the current identity (opened with
/// a sealer that accepts plaintext). Answers how many files it rewrote or removed.
fn reseal_layer(state: &LayerState) -> Result<usize, String> {
    let mut changed = match &state.model {
        ModelState::Channel(channel) => channel.read().expect("channel lock").reseal()?,
        ModelState::Tuplespace(space) => space.reseal()?,
    };
    if state.feed.lock().expect("feed lock").reseal()? {
        changed += 1;
    }
    Ok(changed)
}

fn open_layer(
    def: LayerDef,
    dir: &Path,
    budget: EvaluationBudget,
    egress: serde_json::Value,
    sealer: &Sealer,
) -> Result<LayerState, String> {
    at_rest::private_dir(dir)?;
    let feed_path = dir.join("feed.log");
    feed::touch(&feed_path)?;
    let mut feed = FeedLog::open(&feed_path, sealer)?;
    let model = match def.model {
        Model::Channel => {
            let channel = Channel::open(&def, dir, sealer)?;
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
            let (space, stored) = Tuplespace::open_with_budget(&def, dir, &logged, budget, sealer)?;
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
        egress,
    })
}
