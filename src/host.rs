//! The host: the configuration it was opened with, and each layer's state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use crate::channel::Channel;
use crate::config::{self, Config, ConfigError, LayerDef, Model, Participants};
use crate::feed::{self, Event, FeedLog, Notify};
use crate::tuplespace::Tuplespace;

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

/// A layer host: what [`crate::space`] binds.
///
/// Opened over two directories: the CONFIG directory (definitions, shapes, contexts,
/// participants, manifests: the operator's, read-only here) and the STATE directory (each
/// layer's entries and feed, `<state>/{layer}/`, written only by this host). One host
/// process per state directory: the write lock that keeps a feed gapless is in-process.
///
/// Definitions are read once, at open, because each layer binds its own endpoints; adding
/// or changing a layer means opening the host again. Participants and manifests are read
/// on every use, because the door's enrollment is live (cli re-reads `clients.json` per
/// connection) and a participant enrolled a minute ago must be able to write.
pub struct Host {
    config_dir: PathBuf,
    layers: BTreeMap<String, Arc<LayerState>>,
    notify: Arc<Notify>,
}

impl Host {
    /// Open a host: check the whole configuration (a manifest that breaks the isolation
    /// rule stops the host here, not at its first read), then open every layer's state,
    /// repairing a feed that lost the event of an entry stored just before a crash.
    pub fn open(
        config_dir: impl AsRef<Path>,
        state_dir: impl AsRef<Path>,
    ) -> Result<Arc<Host>, ConfigError> {
        let config = Config::load(&config_dir)?;
        let state_dir = state_dir.as_ref();
        let mut layers = BTreeMap::new();
        for (name, def) in config.layers {
            let dir = state_dir.join(&name);
            let state = open_layer(def, &dir)
                .map_err(|e| ConfigError(format!("layer `{name}` state: {e}")))?;
            layers.insert(name, Arc::new(state));
        }
        Ok(Arc::new(Host {
            config_dir: config.dir,
            layers,
            notify: Arc::new(Notify::default()),
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
        let defs: BTreeMap<String, LayerDef> = self
            .layers
            .iter()
            .map(|(name, state)| (name.clone(), state.def.clone()))
            .collect();
        config::load_collabs(&self.config_dir, &defs, &self.participants()?)
    }
}

fn open_layer(def: LayerDef, dir: &Path) -> Result<LayerState, String> {
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
            let (space, stored) = Tuplespace::open(&def, dir, &logged)?;
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
