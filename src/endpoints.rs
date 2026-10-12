//! The doors: one endpoint per layer per action, each declaring its layer's EXACT
//! capability, plus the host-wide reads.
//!
//! ```text
//! urn:layer:{layer}                Source  layer-{layer}-definition  urn:cap:layer:read:{layer}
//! urn:layer:{layer}:shapes         Source  layer-{layer}-shapes      urn:cap:layer:read:{layer}
//! urn:layer:{layer}:context        Source  layer-{layer}-context     urn:cap:layer:read:{layer}
//! urn:layer:{layer}:feed           Source  layer-{layer}-feed        urn:cap:layer:feed:{layer}
//! urn:layer:{layer}:say            Sink    layer-{layer}-say         urn:cap:layer:say:{layer}
//! urn:layer:{layer}:entries        Source  layer-{layer}-entries     urn:cap:layer:read:{layer}
//! urn:layer:{layer}:entry:{id}     Source  layer-{layer}-entry       urn:cap:layer:read:{layer}
//! urn:layer:{layer}:graph:{slug}   Source  layer-{layer}-graph       urn:cap:layer:read:{layer}
//! urn:layer:{layer}:latest         Source  layer-{layer}-latest      urn:cap:layer:read:{layer}
//! urn:layer:{layer}:tuples         Sink    layer-{layer}-tuples      urn:cap:layer:out:{layer}   (out)
//!                                  Source                            urn:cap:layer:read:{layer}  (rd)
//!                                  Delete                            urn:cap:layer:take:{layer}  (take)
//! urn:layer:{layer}:receipt:{id}   Source  layer-{layer}-receipt     urn:cap:layer:read:{layer}
//! urn:layers:feed                  Source  layers-feed               urn:cap:layer:feed:*  (+ exact per layer)
//! urn:layers:participants          Source  layers-participants       urn:cap:layers:read:participants
//! urn:layers:me                    Source  layers-me                 urn:cap:collab:read:*  (any participant grant)
//! urn:collab:{name}                Source  collab                    urn:cap:collab:read:* (+ exact per manifest)
//! urn:collab:{name}:participants   Source  collab-participants       urn:cap:collab:read:* (+ exact per manifest)
//! ```
//!
//! A channel layer binds `say`, `entries`, `entry`, `graph` and `latest`; a tuplespace layer
//! binds `tuples` and `receipt`; every layer binds its definition, shapes, context and feed.
//! Each declares its JSON face in its `as` argument ([`crate::json`]): `application/ld+json`
//! for a graph, `application/json` for a list or a write's answer.
//!
//! **Why exact, and why per layer.** Core's selection matches a declared scope EXACTLY
//! unless it ends in `*`, and then it prefix-tests what is held after stripping the `*`
//! (`cap_satisfies`). So `urn:cap:layer:say:talk` is satisfied by that token alone (or root):
//! not by `urn:cap:layer:say:talk-bobby`, and not by a held `urn:cap:layer:say:*` either,
//! which is held as a literal string and allows only itself. Binding one endpoint per
//! layer is what lets each declare its own exact token, so the manifold under a
//! participant's grant, and the MCP tool list projected from it, is exactly the layers it
//! holds. The host-wide reads that span names (`urn:layers:feed`, `urn:collab:{name}` and
//! `urn:collab:{name}:participants`) must declare the family and check the exact token
//! inside; they offer themselves to anyone holding some grant of the family, and refuse the
//! rest by name.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ActionSpec, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, ReprType,
    Representation, Result, UriTemplate, Verb,
};
use oxrdf::{Literal, Triple};

use crate::channel::{self, entry_prefix};
use crate::config::{ConfigError, LayerDef, Manifest, Model, Participants};
use crate::feed::{self, Event, MAX_WAIT_SECS};
use crate::host::{Host, LayerState};
use crate::id;
use crate::name::{collab_cap, collab_iri, endpoint_id, layer_cap, layer_iri, Participant};
use crate::rdf::{self, node};
use crate::time::xsd_date_time;
use crate::tuplespace::{self, Lookup, Receipt, TupleMeta};

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const TURTLE: &str = "text/turtle";
const JSON: &str = "application/json";
const JSON_LD: &str = "application/ld+json";
const TEXT: &str = "text/plain";

/// ikigai-shacl's validator, by its resource name. **Admission no longer composes it**
/// (since 0.2.1): the host links the validator and calls it itself, so a door binds the
/// layer space ALONE and offers participants no validator (ledger #1266). Kept for hosts
/// that name it, e.g. to check that a door they built does NOT bind it.
pub const VALIDATE: &str = "urn:shacl:validate";

/// The capability that reads the WHOLE participants graph, `urn:layers:participants`. Not
/// generated for anyone: who takes part in OTHER collaborations is not a participant's
/// business (isolation), so this is the operator's (root's). A participant reads its own
/// collaborations' participants at `urn:collab:{name}:participants`, under the
/// `urn:cap:collab:read:{name}` every generated grant already carries (ledger #1282).
pub const CAP_PARTICIPANTS: &str = "urn:cap:layers:read:participants";

/// `urn:layers:me`: who the door says the caller is.
pub const ME: &str = "urn:layers:me";

/// The family `urn:layers:feed` declares (and then checks exactly, per layer).
pub const CAP_FEED_FAMILY: &str = "urn:cap:layer:feed:*";

/// The family `urn:collab:{name}` declares (and then checks exactly, per manifest).
pub const CAP_COLLAB_FAMILY: &str = "urn:cap:collab:read:*";

/// The most entries one `entries` or `graph:{slug}` read answers by default, and the most
/// it may ask for. A reader pages with `after=` (the last `ik:seq` it received).
pub const DEFAULT_LIMIT: u64 = 100;
/// See [`DEFAULT_LIMIT`]: a larger `limit=` is refused, never quietly lowered.
pub const MAX_LIMIT: u64 = 1000;

/// Bind every layer of `host`, and the host-wide reads, into one space.
///
/// The space is HOST-named: what it answers is the configuration and state it was handed,
/// so it claims no name of its own, and a host that binds it names it (core's
/// `SPACE-NAME` rule, ledger #987).
pub fn space(host: &Arc<Host>) -> EndpointSpace {
    let template = |t: String| UriTemplate::parse(t).expect("layer names are template-safe");
    let mut space = EndpointSpace::new();
    for (name, state) in host.layers() {
        let base = layer_iri(name);
        for action in Action::of(state.def.model) {
            let endpoint = LayerEndpoint {
                host: Arc::clone(host),
                state: Arc::clone(state),
                action,
            };
            space = match action.binding() {
                None => space.bind(Exact::new(action.iri(&base)), endpoint),
                Some(_) => space.bind(template(action.iri(&base)), endpoint),
            };
        }
    }
    space
        .bind(
            Exact::new("urn:layers:feed"),
            MultiFeed {
                host: Arc::clone(host),
            },
        )
        .bind(
            Exact::new("urn:layers:participants"),
            ParticipantsEndpoint {
                host: Arc::clone(host),
            },
        )
        .bind(
            Exact::new(ME),
            MeEndpoint {
                host: Arc::clone(host),
            },
        )
        // Before `urn:collab:{name}`: the first binding that matches wins, and a final
        // `{name}` captures the rest of the IRI, `bobby:participants` included.
        .bind(
            template("urn:collab:{name}:participants".into()),
            CollabParticipants {
                host: Arc::clone(host),
            },
        )
        .bind(
            template("urn:collab:{name}".into()),
            CollabEndpoint {
                host: Arc::clone(host),
            },
        )
}

fn host_error(e: ConfigError) -> Error {
    Error::Endpoint(e.to_string())
}

fn xsd(local: &str) -> String {
    format!("{XSD}{local}")
}

fn optional_str<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<Option<&'a str>> {
    match inv.request.args.get(name) {
        None => Ok(None),
        Some(_) => inv.inline_str(name).map(Some),
    }
}

fn optional_u64(inv: &Invocation<'_>, name: &str) -> Result<Option<u64>> {
    match optional_str(inv, name)? {
        None => Ok(None),
        Some(text) => text
            .trim()
            .parse()
            .map(Some)
            .map_err(|_| Error::InvalidArgument {
                name: name.into(),
                detail: format!("`{text}` is not a non-negative integer"),
            }),
    }
}

fn wait_of(inv: &Invocation<'_>) -> Result<Duration> {
    let wait = optional_u64(inv, "wait")?.unwrap_or(0);
    if wait > MAX_WAIT_SECS {
        return Err(Error::InvalidArgument {
            name: "wait".into(),
            detail: format!("{wait} seconds is over the bound of {MAX_WAIT_SECS}"),
        });
    }
    Ok(Duration::from_secs(wait))
}

fn face<'a>(inv: &'a Invocation<'_>, faces: &[&'a str]) -> Result<&'a str> {
    match optional_str(inv, "as")? {
        None => Ok(faces[0]),
        Some(asked) => {
            faces
                .iter()
                .copied()
                .find(|f| *f == asked)
                .ok_or_else(|| Error::InvalidArgument {
                    name: "as".into(),
                    detail: format!("`{asked}` is not one of {}", faces.join(", ")),
                })
        }
    }
}

fn as_spec(faces: &[&str]) -> ArgSpec {
    ArgSpec::new("as")
        .summary("the representation to answer with")
        .class(xsd("string"))
        .one_of(faces.iter().copied())
        .default_value(faces[0])
}

fn after_spec(summary: &str) -> ArgSpec {
    ArgSpec::new("after")
        .summary(summary)
        .class(xsd("nonNegativeInteger"))
}

fn wait_spec() -> ArgSpec {
    ArgSpec::new("wait")
        .summary(format!(
            "seconds to wait for an event when there is none yet, at most {MAX_WAIT_SECS} (a \
             long poll: no follower needs to poll)"
        ))
        .class(xsd("nonNegativeInteger"))
        .default_value("0")
}

fn limit_spec() -> ArgSpec {
    ArgSpec::new("limit")
        .summary(format!(
            "the most entries to answer, at most {MAX_LIMIT}; page with after= (the last \
             ik:seq received)"
        ))
        .class(xsd("nonNegativeInteger"))
        .default_value(DEFAULT_LIMIT.to_string())
}

fn turtle(text: String) -> Representation {
    Representation::new(ReprType::new(TURTLE), text.into_bytes())
}

fn json_repr(body: serde_json::Value) -> Representation {
    Representation::new(ReprType::new(JSON), body.to_string().into_bytes())
}

/// A write's answer: the IRI it named, as text or as `{"@id": …}`.
fn named(iri: &str, as_json: bool) -> Representation {
    if as_json {
        json_repr(serde_json::json!({ "@id": iri }))
    } else {
        Representation::new(ReprType::new(TEXT), iri.as_bytes().to_vec())
    }
}

/// The faces of a graph answer: Turtle, as stored, or JSON-LD (see [`crate::json`]).
const GRAPH_FACES: [&str; 2] = [TURTLE, JSON_LD];

/// `rd`'s faces: the list as text (the default) or JSON, the graphs as Turtle or JSON-LD.
const RD_FACES: [&str; 4] = [TEXT, TURTLE, JSON, JSON_LD];

/// One layer action. Each is its own endpoint, with its own id and exact capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Definition,
    Shapes,
    Context,
    Feed,
    Say,
    Entries,
    Entry,
    Graph,
    Latest,
    Tuples,
    Receipt,
}

impl Action {
    /// Every layer's actions, then its model's.
    fn of(model: Model) -> Vec<Action> {
        let mut actions = vec![
            Action::Definition,
            Action::Shapes,
            Action::Context,
            Action::Feed,
        ];
        actions.extend_from_slice(match model {
            Model::Channel => &[
                Action::Say,
                Action::Entries,
                Action::Entry,
                Action::Graph,
                Action::Latest,
            ][..],
            Model::Tuplespace => &[Action::Tuples, Action::Receipt][..],
        });
        actions
    }

    fn word(self) -> &'static str {
        match self {
            Action::Definition => "definition",
            Action::Shapes => "shapes",
            Action::Context => "context",
            Action::Feed => "feed",
            Action::Say => "say",
            Action::Entries => "entries",
            Action::Entry => "entry",
            Action::Graph => "graph",
            Action::Latest => "latest",
            Action::Tuples => "tuples",
            Action::Receipt => "receipt",
        }
    }

    /// The template variable this action's IRI captures, if any.
    fn binding(self) -> Option<&'static str> {
        match self {
            Action::Entry => Some("id"),
            Action::Graph => Some("slug"),
            Action::Receipt => Some("id"),
            _ => None,
        }
    }

    fn iri(self, base: &str) -> String {
        match self {
            Action::Definition => base.to_string(),
            Action::Entry => format!("{base}:entry:{{id}}"),
            Action::Graph => format!("{base}:graph:{{slug}}"),
            Action::Receipt => format!("{base}:receipt:{{id}}"),
            other => format!("{base}:{}", other.word()),
        }
    }

    /// The `{verb}` of the capability this action requires (for `tuples`, its Meta face; each
    /// of its three verbs declares its own).
    fn cap_verb(self) -> &'static str {
        match self {
            Action::Feed => "feed",
            Action::Say => "say",
            _ => "read",
        }
    }
}

struct LayerEndpoint {
    host: Arc<Host>,
    state: Arc<LayerState>,
    action: Action,
}

impl LayerEndpoint {
    fn def(&self) -> &LayerDef {
        &self.state.def
    }

    fn layer(&self) -> &str {
        &self.state.def.name
    }

    /// The thread every read of the layer's entries hangs from: the `say` door's own IRI,
    /// which the kernel cuts after every Sink through it. So the reads are cacheable, and a
    /// new entry invalidates exactly them.
    fn say_thread(&self) -> String {
        format!("{}:say", layer_iri(self.layer()))
    }

    /// A NotFound that a later `say` can end: it hangs from the say thread too (ledger
    /// #1079), or a cached absence would outlive the entry that ends it.
    fn not_found(&self, inv: &Invocation<'_>, what: String) -> Error {
        inv.depends_on(self.say_thread());
        Error::NotFound(what)
    }

    /// A graph answer in the face asked for: the stored Turtle, or the layer's egress context
    /// pruned and compacted by the host's own processor ([`crate::json`]).
    async fn graph_face(&self, text: String, as_ld: bool) -> Result<Representation> {
        if !as_ld {
            return Ok(turtle(text));
        }
        let triples = rdf::parse_turtle(&text).map_err(|e| {
            Error::Endpoint(format!(
                "layer `{}`: a stored graph does not parse: {e}",
                self.layer()
            ))
        })?;
        let doc = self
            .host
            .json()
            .compact(&self.state.egress, &triples)
            .await
            .map_err(|e| {
                Error::Endpoint(format!("layer `{}`: its JSON-LD face: {e}", self.layer()))
            })?;
        Ok(Representation::new(
            ReprType::new(JSON_LD),
            doc.to_string().into_bytes(),
        ))
    }

    fn definition(&self) -> Representation {
        let def = self.def();
        let base = layer_iri(&def.name);
        let s = node(&base);
        let mut triples = vec![
            Triple::new(s.clone(), node(rdf::RDF_TYPE), node(&rdf::ik_layer_class())),
            Triple::new(
                s.clone(),
                node(rdf::RDFS_LABEL),
                Literal::new_simple_literal(def.label.as_deref().unwrap_or(&def.name)),
            ),
            Triple::new(
                s.clone(),
                node(&rdf::ik_layer_model()),
                Literal::new_simple_literal(def.model.as_str()),
            ),
        ];
        let parts: &[&str] = match def.model {
            Model::Channel => &["shapes", "context", "feed"],
            Model::Tuplespace => &["shapes", "context", "feed", "tuples"],
        };
        for part in parts {
            triples.push(Triple::new(
                s.clone(),
                node(rdf::RDFS_SEE_ALSO),
                node(&format!("{base}:{part}")),
            ));
        }
        for collab in def.shared.iter().flatten() {
            triples.push(Triple::new(
                s.clone(),
                node(rdf::DCT_IS_PART_OF),
                node(&collab_iri(collab)),
            ));
        }
        turtle(rdf::to_turtle(&triples)).cacheable()
    }

    fn feed(&self, events: &[Event], as_json: bool) -> Representation {
        if as_json {
            let body = serde_json::json!({
                "layer": self.layer(),
                "last": self.state.last_seq(),
                "events": events,
            });
            Representation::new(ReprType::new(JSON), body.to_string().into_bytes())
        } else {
            let text: String = events
                .iter()
                .map(|e| format!("{} {} {} {} {}\n", e.seq, e.time, e.kind, e.iri, e.by))
                .collect();
            Representation::new(ReprType::new(TEXT), text.into_bytes())
        }
    }

    async fn read_feed(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let after =
            optional_u64(inv, "after")?.ok_or_else(|| Error::MissingArgument("after".into()))?;
        let wait = wait_of(inv)?;
        let as_json = face(inv, &[TEXT, JSON])? == JSON;
        let last = self.state.last_seq();
        if after > last {
            return Err(Error::Conflict(format!(
                "cursor {after} is past layer `{}`'s last event, {last}: this host's feed was \
                 not the one the cursor came from (restored, or another host); resynchronize \
                 from 0",
                self.layer()
            )));
        }
        let state = &self.state;
        feed::wait_until(self.host.notify(), wait, || state.last_seq() > after).await;
        Ok(self.feed(&self.state.events_after(after), as_json))
    }

    /// The caller's principal, which must be a participant: every write is attributed.
    fn attributed(&self, inv: &Invocation<'_>, verb: &str) -> Result<(String, Participants)> {
        let principal = inv
            .capability
            .principal()
            .ok_or_else(|| {
                Error::Denied(format!(
                    "`{verb}` on layer `{}` needs a principal, minted by the door that \
                     authenticated the caller: every write is attributed, and this request \
                     names nobody (root names nobody either)",
                    self.layer()
                ))
            })?
            .to_string();
        let participants = self.host.participants().map_err(host_error)?;
        if !participants.names(&principal) {
            return Err(Error::Denied(format!(
                "{principal} is not a participant of this host (participants.ttl does not name \
                 it)"
            )));
        }
        Ok((principal, participants))
    }

    fn now(&self, inv: &Invocation<'_>) -> Result<ikigai_core::Time> {
        inv.now().ok_or_else(|| {
            Error::Endpoint(
                "this host gives the kernel no clock, and every write is stamped with its time"
                    .into(),
            )
        })
    }

    /// Under the layer's write lock, before a write takes its sequence number: log what an
    /// earlier write stored but could not log (ledger #1218), and answer the number this
    /// write carries. When the feed still cannot be written the write is refused here,
    /// before it stores anything, so no second write can be stamped with a spent number and
    /// nothing piles up unlogged.
    fn settle_feed(&self) -> Result<u64> {
        let (settled, next, owed) = {
            let mut feed = self.state.feed.lock().expect("feed lock");
            let settled = feed.settle();
            let owed = feed
                .first_owed()
                .map(|e| format!("{} <{}> by <{}>, ik:seq {}", e.kind, e.iri, e.by, e.seq));
            (settled, feed.next(), owed)
        };
        match settled {
            Ok(0) => Ok(next),
            Ok(_) => {
                self.host.notify().bump();
                Ok(next)
            }
            Err(e) => Err(Error::Endpoint(format!(
                "layer `{}`'s feed cannot be written ({e}): a stored write's event ({}) is not \
                 logged yet, and the layer takes no write until it is",
                self.layer(),
                owed.as_deref().unwrap_or("unknown")
            ))),
        }
    }

    /// Log a stored write's event. When the feed cannot be written the event is OWED: its
    /// number stays spent, and the next write (or the next open) logs it first.
    fn log_stored(&self, event: Event) -> std::result::Result<(), String> {
        let mut feed = self.state.feed.lock().expect("feed lock");
        feed.append(event.clone()).inspect_err(|_| feed.owe(event))
    }

    async fn say(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // The face first: a write answered in a face nobody offers is refused before it is
        // stored, never after.
        let as_json = face(inv, &[TEXT, JSON])? == JSON;
        let (author, _) = self.attributed(inv, "say")?;
        let draft = channel::draft(self.def(), inv.inline_arg("content")?)?;
        // From here to the feed append is one step per layer: the sequence number is
        // assigned, stamped, admitted, stored and logged under this lock.
        let _write = self.state.write.lock().await;
        let seq = self.settle_feed()?;
        self.state
            .channel()
            .read()
            .expect("channel lock")
            .check_supersedes(&draft, &author)?;
        let now = self.now(inv)?;
        let iri = format!(
            "{}{}",
            entry_prefix(self.layer()),
            id::mint(now.as_millis(), seq)
        );
        let created = xsd_date_time(now);
        let stamped = draft.stamp(&iri, &author, &created, seq);
        admit(self.def(), &stamped)?;
        self.state
            .channel()
            .write()
            .expect("channel lock")
            .append(&stamped)
            .map_err(Error::Endpoint)?;
        let logged = self.log_stored(Event {
            seq,
            time: created,
            kind: "say".into(),
            iri: iri.clone(),
            by: author,
        });
        drop(_write);
        if let Err(e) = logged {
            return Err(Error::Endpoint(format!(
                "entry <{iri}> is stored, with ik:seq {seq}, but its feed event could not be \
                 logged ({e}): it is logged before the layer's next event, or when the host \
                 next opens. Saying it again would store it twice"
            )));
        }
        self.host.notify().bump();
        Ok(named(&iri, as_json))
    }

    fn page(&self, inv: &Invocation<'_>) -> Result<(u64, u64)> {
        let after = optional_u64(inv, "after")?.unwrap_or(0);
        let limit = optional_u64(inv, "limit")?.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 || limit > MAX_LIMIT {
            return Err(Error::InvalidArgument {
                name: "limit".into(),
                detail: format!("{limit} is outside 1..={MAX_LIMIT}; page with after="),
            });
        }
        Ok((after, limit))
    }

    /// The entries the filter keeps, as one Turtle document (each entry is stored as its
    /// own Turtle document, and Turtle documents concatenate), or as JSON-LD.
    async fn entries_where(
        &self,
        inv: &Invocation<'_>,
        keep: impl Fn(&channel::EntryMeta) -> bool,
    ) -> Result<Representation> {
        let as_ld = face(inv, &GRAPH_FACES)? == JSON_LD;
        let (after, limit) = self.page(inv)?;
        let text = {
            let channel = self.state.channel().read().expect("channel lock");
            let mut text = String::new();
            for meta in channel
                .entries()
                .iter()
                .filter(|m| m.seq > after && keep(m))
                .take(limit as usize)
            {
                text.push_str(&channel.turtle(meta).map_err(Error::Endpoint)?);
            }
            text
        };
        Ok(self
            .graph_face(text, as_ld)
            .await?
            .cacheable()
            .depends_on(self.say_thread()))
    }

    async fn entries(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let from = match optional_str(inv, "from")? {
            None => None,
            Some(iri) => Some(
                Participant::parse(iri)
                    .map_err(|detail| Error::InvalidArgument {
                        name: "from".into(),
                        detail,
                    })?
                    .iri()
                    .to_string(),
            ),
        };
        let class = optional_str(inv, "type")?.map(str::to_string);
        self.entries_where(inv, |m| {
            from.as_ref().is_none_or(|a| &m.author == a)
                && class.as_ref().is_none_or(|c| m.types.contains(c))
        })
        .await
    }

    async fn entry(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let as_ld = face(inv, &GRAPH_FACES)? == JSON_LD;
        let id = inv.bindings.get("id").unwrap_or_default();
        let found = {
            let channel = self.state.channel().read().expect("channel lock");
            match channel.get(id) {
                Some(meta) => Some(channel.turtle(meta).map_err(Error::Endpoint)?),
                None => None,
            }
        };
        match found {
            // An entry never changes: cacheable for as long as the layer exists.
            Some(text) => Ok(self.graph_face(text, as_ld).await?.cacheable()),
            None => {
                Err(self.not_found(inv, format!("no entry `{id}` in layer `{}`", self.layer())))
            }
        }
    }

    async fn graph(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let slug = inv.bindings.get("slug").unwrap_or_default().to_string();
        self.entries_where(inv, |m| m.slug == slug).await
    }

    async fn latest(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let as_ld = face(inv, &GRAPH_FACES)? == JSON_LD;
        let about = inv.inline_str("about")?;
        let found = {
            let channel = self.state.channel().read().expect("channel lock");
            match channel.latest(about) {
                Some(meta) => Some(channel.turtle(meta).map_err(Error::Endpoint)?),
                None => None,
            }
        };
        match found {
            Some(text) => Ok(self
                .graph_face(text, as_ld)
                .await?
                .cacheable()
                .depends_on(self.say_thread())),
            None => Err(self.not_found(
                inv,
                format!(
                    "no entry in layer `{}` is about {about} without being superseded",
                    self.layer()
                ),
            )),
        }
    }

    // ---- the tuplespace model --------------------------------------------------------

    /// The thread every read of a tuplespace hangs from: the `tuples` door's own IRI, which
    /// the kernel cuts after every Sink (out) and Delete (take) through it.
    fn tuples_thread(&self) -> String {
        format!("{}:tuples", layer_iri(self.layer()))
    }

    /// A store failure is the host's, never the caller's argument.
    fn space_error(&self, e: impl std::fmt::Display) -> Error {
        Error::Endpoint(format!("layer `{}`: its space: {e}", self.layer()))
    }

    /// `out`: drop one tuple, named by its one subject, stamped, admitted by the layer's
    /// closed shapes; answers its subject.
    async fn out(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // The face first, as for `say`.
        let as_json = face(inv, &[TEXT, JSON])? == JSON;
        let (dropper, participants) = self.attributed(inv, "out")?;
        let draft = tuplespace::draft(self.def(), inv.inline_arg("content")?)?;
        if let Some(to) = draft.addressed_to.iter().find(|p| !participants.names(p)) {
            return Err(Error::InvalidArgument {
                name: "content".into(),
                detail: format!(
                    "ik:addressedTo <{to}> names no participant of this host, so nobody could \
                     take the tuple"
                ),
            });
        }
        let space = self.state.tuplespace();
        let answer = |subject: &str| named(subject, as_json);
        // From here to the feed append is one step per layer, as for `say`.
        let _write = self.state.write.lock().await;
        {
            let index = space.index.read().expect("tuplespace lock");
            match index.lookup(&draft.subject) {
                Lookup::Unknown => {}
                // The same drop again (a retry after a lost reply): one record, answered as
                // the drop it repeats, with no new event.
                Lookup::Pending(meta) if draft.replays(meta, &dropper) => {
                    return Ok(answer(&draft.subject))
                }
                Lookup::Taken(receipt) if draft.replays(&receipt.tuple, &dropper) => {
                    return Ok(answer(&draft.subject))
                }
                _ => {
                    return Err(Error::Conflict(format!(
                        "<{}> already names a tuple of layer `{}` (a subject names one tuple, \
                         pending or taken, for the life of the layer): mint a new one",
                        draft.subject,
                        self.layer()
                    )))
                }
            }
            if let Some(offer) = &draft.offer {
                check_report(self.def(), &index, offer, &dropper, &draft.addressed_to)?;
            }
        }
        let seq = self.settle_feed()?;
        let now = self.now(inv)?;
        let created = xsd_date_time(now);
        let stamped = draft.stamp(&dropper, &created, seq);
        admit(self.def(), &stamped)?;
        let cid = space
            .store
            .drop_tuple(rdf::to_turtle(&stamped).as_bytes())
            .await
            .map_err(|e| self.space_error(e))?;
        let meta = draft.meta(&cid, &dropper, &created, seq);
        let event = tuplespace::out_event(&meta);
        let logged = self
            .state
            .feed
            .lock()
            .expect("feed lock")
            .append(event.clone());
        if let Err(e) = logged {
            // Never acknowledged and never logged: take it back out, so the space holds only
            // what the feed records, and its number is free again.
            match space.store.take(&cid).await {
                Ok(_) => return Err(Error::Endpoint(e)),
                Err(back) => {
                    // Still in the space, so its number is spent (ledger #1218): owed to the
                    // feed, and recorded as the pending tuple it is.
                    self.state.feed.lock().expect("feed lock").owe(event);
                    space.record_out(meta);
                    return Err(Error::Endpoint(format!(
                        "<{}> is dropped, with ik:seq {seq}, but its feed event could not be \
                         logged ({e}) and it could not be taken back ({back}): it is logged \
                         before the layer's next event, or when the host next opens",
                        draft.subject
                    )));
                }
            }
        }
        space.record_out(meta);
        drop(_write);
        self.host.notify().bump();
        Ok(answer(&draft.subject))
    }

    /// `take`: remove one pending tuple, by its subject, atomically; the receipt naming the
    /// taker is committed before the reply. Answers the tuple as it was taken.
    async fn take(&self, inv: &Invocation<'_>) -> Result<Representation> {
        // The face first: a take answered in a face nobody offers is refused before it takes.
        let as_ld = face(inv, &GRAPH_FACES)? == JSON_LD;
        let (taker, _) = self.attributed(inv, "take")?;
        let subject = take_subject(inv)?;
        let space = self.state.tuplespace();
        let _write = self.state.write.lock().await;
        let meta: TupleMeta = {
            let index = space.index.read().expect("tuplespace lock");
            match index.lookup(&subject) {
                Lookup::Pending(meta) => meta.clone(),
                Lookup::Taken(receipt) => return Err(already_taken(&subject, receipt)),
                Lookup::Gone => return Err(self.gone(&subject)),
                Lookup::Unknown => {
                    return Err(Error::NotFound(format!(
                        "no tuple <{subject}> in layer `{}`",
                        self.layer()
                    )))
                }
            }
        };
        if self.def().addressed
            && !meta.addressed_to.is_empty()
            && !meta.addressed_to.contains(&taker)
        {
            return Err(Error::Denied(format!(
                "<{subject}> is addressed to {}: in layer `{}` only an addressee takes it",
                meta.addressed_to
                    .iter()
                    .map(|p| format!("<{p}>"))
                    .collect::<Vec<_>>()
                    .join(", "),
                self.layer()
            )));
        }
        let seq = self.settle_feed()?;
        // Read the tuple first: its receipt is staged, holding it, before the take.
        let Some(bytes) = space
            .store
            .read(&meta.cid)
            .await
            .map_err(|e| self.space_error(e))?
        else {
            space.record_gone(&subject);
            return Err(self.gone(&subject));
        };
        let text = String::from_utf8(bytes)
            .map_err(|_| self.space_error(format!("tuple {} is not UTF-8", meta.cid)))?;
        let tuple = rdf::parse_turtle(&text).map_err(|e| self.space_error(e))?;
        let now = self.now(inv)?;
        let time = xsd_date_time(now);
        let id = id::mint(now.as_millis(), seq);
        let (_, document) =
            tuplespace::receipt_document(self.layer(), &id, &taker, &time, seq, &subject, &tuple);
        let receipt: Receipt = tuplespace::receipt_of(self.layer(), &document)?;
        space
            .store
            .stage(&meta.cid, &id, &document)
            .map_err(|e| self.space_error(e))?;
        // The atomic step: intray's rename. Losing it here means something other than this
        // host took the tuple (this host's own takers are serialized above).
        match space.store.take(&meta.cid).await {
            Ok(Some(_)) => {}
            Ok(None) => {
                space.store.unstage(&meta.cid, &id);
                space.record_gone(&subject);
                return Err(self.gone(&subject));
            }
            Err(e) => {
                space.store.unstage(&meta.cid, &id);
                return Err(self.space_error(e));
            }
        }
        // The take is done and cannot be undone: whatever the commit and the feed say, it is
        // recorded, and its number is spent (ledger #1218, #1221). A receipt that cannot be
        // committed stays staged, which is a record too: reads fall back to it, and the next
        // open commits it (its tuple has left the space).
        let committed = space.store.commit(&meta.cid, &id);
        let logged = self.log_stored(tuplespace::taken_event(&receipt));
        let receipt_iri = receipt.iri.clone();
        space.record_taken(receipt);
        drop(_write);
        let mut failures = Vec::new();
        if let Err(e) = committed {
            failures.push(format!(
                "its receipt could not be committed ({e}), so it is read from where it is \
                 staged and committed when the host next opens"
            ));
        }
        if let Err(e) = logged {
            failures.push(format!(
                "its feed event could not be logged ({e}), so it is logged before the layer's \
                 next event, or when the host next opens"
            ));
        }
        if !failures.is_empty() {
            return Err(Error::Endpoint(format!(
                "<{subject}> is taken by <{taker}> (receipt <{receipt_iri}>, ik:seq {seq}) and \
                 the take stands, but {}",
                failures.join("; and ")
            )));
        }
        self.host.notify().bump();
        // The take stands whatever this answers: a JSON-LD face that failed here (its context
        // was checked when the host opened) leaves the taker reading it back from its receipt.
        self.graph_face(text, as_ld).await
    }

    /// `rd match=` on a layer whose definition does not say `match = true` (ledger #1327).
    fn no_match(&self) -> Error {
        Error::InvalidArgument {
            name: "match".into(),
            detail: format!(
                "layer `{}` does not take match=: its definition does not set `match = true`. \
                 A template is a SPARQL ASK evaluated against every pending tuple; its caller \
                 is answered within the host's time budget, but one the evaluator cannot stop \
                 holds a core until it ends, so a layer opts in only when it trusts its readers \
                 with that; read the pending tuples without one (rd, or rd tuple=) and select \
                 them yourself",
                self.layer()
            ),
        }
    }

    fn gone(&self, subject: &str) -> Error {
        Error::Conflict(format!(
            "<{subject}> left layer `{}`'s space without a take through this host, so there is \
             no receipt and nobody can take it",
            self.layer()
        ))
    }

    /// `rd`: list or read the pending tuples (or, with `state=taken`, the receipts).
    ///
    /// Four faces: `text/plain` and `application/json` LIST (one line, or one JSON object,
    /// per tuple or receipt); `text/turtle` and `application/ld+json` are the GRAPHS. One
    /// tuple (`tuple=`) is always a graph: Turtle under either text face, as before, and
    /// refused as `application/json`, which is a list's face.
    async fn rd(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let state = optional_str(inv, "state")?.unwrap_or("pending");
        let taken = match state {
            "pending" => false,
            "taken" => true,
            other => {
                return Err(Error::InvalidArgument {
                    name: "state".into(),
                    detail: format!("`{other}` is not one of pending, taken"),
                })
            }
        };
        let asked = face(inv, &RD_FACES)?;
        let (after, limit) = self.page(inv)?;
        let tuple = optional_str(inv, "tuple")?;
        let template = optional_str(inv, "match")?;
        if template.is_some() && !self.def().matching {
            return Err(self.no_match());
        }
        let space = self.state.tuplespace();
        let answer = |r: Representation| Ok(r.cacheable().depends_on(self.tuples_thread()));
        if let (Some(_), Some(_)) = (tuple, template) {
            return Err(Error::InvalidArgument {
                name: "match".into(),
                detail: "tuple= names one tuple and match= selects by a template: give one".into(),
            });
        }
        if let Some(subject) = tuple {
            if asked == JSON {
                return Err(Error::InvalidArgument {
                    name: "as".into(),
                    detail: format!(
                        "tuple= answers one graph, as {TURTLE} or {JSON_LD}; {JSON} is the \
                         face of a list (rd without tuple=)"
                    ),
                });
            }
            let as_ld = asked == JSON_LD;
            let found = {
                let index = space.index.read().expect("tuplespace lock");
                match (taken, index.lookup(subject)) {
                    (false, Lookup::Pending(meta)) => Ok(Err(meta.cid.clone())),
                    (true, Lookup::Taken(receipt)) => Ok(Ok(receipt.id.clone())),
                    (false, Lookup::Taken(receipt)) => Err(format!(
                        "<{subject}> is not pending: <{}> took it at {} (read its receipt with \
                         state=taken)",
                        receipt.taker, receipt.time
                    )),
                    (true, Lookup::Pending(_)) => {
                        Err(format!("<{subject}> is pending: nobody has taken it"))
                    }
                    (_, Lookup::Gone) => Err(format!(
                        "<{subject}> left the space without a take through this host"
                    )),
                    (_, Lookup::Unknown) => {
                        Err(format!("no tuple <{subject}> in layer `{}`", self.layer()))
                    }
                }
            };
            return match found {
                Ok(Ok(receipt_id)) => {
                    let text = space
                        .store
                        .receipt_text(&receipt_id)
                        .map_err(|e| self.space_error(e))?;
                    answer(self.graph_face(text, as_ld).await?)
                }
                Ok(Err(cid)) => match space
                    .store
                    .read(&cid)
                    .await
                    .map_err(|e| self.space_error(e))?
                {
                    Some(bytes) => {
                        let text = String::from_utf8(bytes)
                            .map_err(|_| self.space_error(format!("tuple {cid} is not UTF-8")))?;
                        answer(self.graph_face(text, as_ld).await?)
                    }
                    // Taken a moment ago: the take's own cut ends this answer.
                    None => Err(self.not_found_tuple(inv, format!("<{subject}> was just taken"))),
                },
                Err(why) => Err(self.not_found_tuple(inv, why)),
            };
        }
        let graph_face = asked == TURTLE || asked == JSON_LD;
        if taken {
            if template.is_some() {
                return Err(Error::InvalidArgument {
                    name: "match".into(),
                    detail: "a template matches pending tuples; receipts are read with tuple= or \
                             listed"
                        .into(),
                });
            }
            let receipts: Vec<Receipt> = space
                .index
                .read()
                .expect("tuplespace lock")
                .receipts()
                .iter()
                .filter(|r| r.seq > after)
                .take(limit as usize)
                .cloned()
                .collect();
            if asked == JSON {
                let listed: Vec<serde_json::Value> = receipts
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "seq": r.seq, "time": r.time, "subject": r.tuple.subject,
                            "taker": r.taker, "receipt": r.iri,
                        })
                    })
                    .collect();
                return answer(json_repr(serde_json::json!({
                    "layer": self.layer(),
                    "receipts": listed,
                })));
            }
            let mut out = String::new();
            for r in &receipts {
                if graph_face {
                    out.push_str(
                        &space
                            .store
                            .receipt_text(&r.id)
                            .map_err(|e| self.space_error(e))?,
                    );
                } else {
                    out.push_str(&format!(
                        "{} {} {} {}\n",
                        r.seq, r.time, r.tuple.subject, r.taker
                    ));
                }
            }
            return answer(if graph_face {
                self.graph_face(out, asked == JSON_LD).await?
            } else {
                Representation::new(ReprType::new(TEXT), out.into_bytes())
            });
        }
        let matched: Option<std::collections::HashSet<String>> = match template {
            None => None,
            Some(query) => Some(space.store.matching(query).await?.into_iter().collect()),
        };
        let pending: Vec<TupleMeta> = space
            .index
            .read()
            .expect("tuplespace lock")
            .pending()
            .filter(|m| m.seq > after)
            .filter(|m| matched.as_ref().is_none_or(|hits| hits.contains(&m.cid)))
            .take(limit as usize)
            .cloned()
            .collect();
        if asked == JSON {
            // The list, mirroring the feed's JSON: what the text face says, one object a line.
            let listed: Vec<serde_json::Value> = pending
                .iter()
                .map(|m| {
                    serde_json::json!({
                        "seq": m.seq, "created": m.created, "subject": m.subject,
                        "dropper": m.dropper,
                    })
                })
                .collect();
            return answer(json_repr(serde_json::json!({
                "layer": self.layer(),
                "tuples": listed,
            })));
        }
        let mut out = String::new();
        for meta in &pending {
            if graph_face {
                // A tuple taken between the index read and here is simply not listed: the
                // take's cut ends this answer.
                if let Some(bytes) = space
                    .store
                    .read(&meta.cid)
                    .await
                    .map_err(|e| self.space_error(e))?
                {
                    out.push_str(&String::from_utf8_lossy(&bytes));
                }
            } else {
                out.push_str(&format!(
                    "{} {} {} {}\n",
                    meta.seq, meta.created, meta.subject, meta.dropper
                ));
            }
        }
        answer(if graph_face {
            self.graph_face(out, asked == JSON_LD).await?
        } else {
            Representation::new(ReprType::new(TEXT), out.into_bytes())
        })
    }

    /// A NotFound that a later out or take can end: it hangs from the tuples thread.
    fn not_found_tuple(&self, inv: &Invocation<'_>, what: String) -> Error {
        inv.depends_on(self.tuples_thread());
        Error::NotFound(what)
    }

    async fn receipt(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let as_ld = face(inv, &GRAPH_FACES)? == JSON_LD;
        let id = inv.bindings.get("id").unwrap_or_default();
        let space = self.state.tuplespace();
        let found = space
            .index
            .read()
            .expect("tuplespace lock")
            .receipt(id)
            .map(|r| r.id.clone());
        match found {
            // A receipt never changes: cacheable for as long as the layer exists.
            Some(id) => {
                let text = space
                    .store
                    .receipt_text(&id)
                    .map_err(|e| self.space_error(e))?;
                Ok(self.graph_face(text, as_ld).await?.cacheable())
            }
            None => Err(self.not_found_tuple(
                inv,
                format!("no receipt `{id}` in layer `{}`", self.layer()),
            )),
        }
    }
}

/// Admission: the stamped entry must conform to the layer's closed shapes, judged by
/// ikigai-shacl's validator, which the host LINKS and calls itself (ledger #1266).
///
/// - Not a sub-request to `urn:shacl:validate`: that endpoint declares no capability, so a
///   door that bound it for admission's sake offered it to every enrolled participant.
///   Called here, it is the host's own step, the way a tuplespace's intray space is, and the
///   door's kernel binds the layer space alone. Nothing reaches the validator but a write
///   the host is admitting, and no authority is involved: validation needs none.
/// - The validator answers a report for data that does NOT conform (`conforms: false`), so
///   admission reads `conforms`, never the absence of an error.
/// - The shapes are the ones the host holds, skolemized, which is exactly what
///   `urn:layer:{layer}:shapes` serves, so the shapes a participant reads are the shapes
///   that judged its entry. They compiled when the host opened ([`crate::Host::open`]).
/// - A failure of the validator itself is the HOST's error, never the writer's bad
///   argument, and nothing is admitted.
fn admit(def: &LayerDef, stamped: &[Triple]) -> Result<()> {
    let report =
        ikigai_shacl::validate_report(&rdf::to_turtle(stamped), &def.shapes).map_err(|e| {
            Error::Endpoint(format!(
                "layer `{}`: admission could not run ({e}); the shapes are the operator's",
                def.name
            ))
        })?;
    if report.conforms {
        return Ok(());
    }
    // The report's JSON face, the one `urn:shacl:validate` answers with `as=application/json`.
    let report = serde_json::to_value(&report).map_err(|e| {
        Error::Endpoint(format!(
            "layer `{}`: the validation report did not serialize: {e}",
            def.name
        ))
    })?;
    Err(Error::InvalidArgument {
        name: "content".into(),
        detail: violations(&def.name, &report),
    })
}

/// At most 20 violations, and a count of the rest.
fn violations(layer: &str, report: &serde_json::Value) -> String {
    const SHOWN: usize = 20;
    let all = report
        .get("violations")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut lines: Vec<String> = all
        .iter()
        .take(SHOWN)
        .map(|v| {
            let path = v.get("path").and_then(|p| p.as_str()).unwrap_or("(node)");
            let message = v
                .get("message")
                .map(|m| match m {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Object(o) => o
                        .get("value")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let component = v
                .get("component")
                .and_then(|c| c.as_str())
                .map(|c| c.rsplit('#').next().unwrap_or(c))
                .unwrap_or_default();
            format!("{path}: {component} {message}").trim().to_string()
        })
        .collect();
    if all.len() > SHOWN {
        lines.push(format!("and {} more", all.len() - SHOWN));
    }
    format!(
        "does not conform to layer `{layer}`'s shapes ({} violation(s)): {}",
        all.len(),
        lines.join("; ")
    )
}

/// Who may report on `offer`: only the participant this layer's receipt says took it,
/// otherwise anyone holding `out` could hand back work they never took. And in an addressed
/// layer, the report is addressed to the offer's offerer and nobody else, or nobody who
/// should could take it.
fn check_report(
    def: &LayerDef,
    index: &tuplespace::Index,
    offer: &str,
    reporter: &str,
    addressed_to: &std::collections::BTreeSet<String>,
) -> Result<()> {
    let layer = def.name.as_str();
    let receipt = match index.lookup(offer) {
        Lookup::Taken(receipt) => receipt,
        Lookup::Pending(_) => {
            return Err(Error::Conflict(format!(
                "ik:offer <{offer}> is still pending in layer `{layer}`: take it before \
                 reporting on it"
            )))
        }
        Lookup::Gone | Lookup::Unknown => {
            return Err(Error::InvalidArgument {
                name: "content".into(),
                detail: format!(
                    "ik:offer <{offer}> is no tuple layer `{layer}` has a receipt for, so there \
                     is no take to report on"
                ),
            })
        }
    };
    if receipt.taker != reporter {
        return Err(Error::Denied(format!(
            "ik:offer <{offer}> was taken by <{}>: only its taker reports on it",
            receipt.taker
        )));
    }
    let offerer = &receipt.tuple.dropper;
    if def.addressed && (addressed_to.len() != 1 || !addressed_to.contains(offerer)) {
        return Err(Error::InvalidArgument {
            name: "content".into(),
            detail: format!(
                "a report on <{offer}> is addressed to its offerer, <{offerer}>, and nobody else \
                 (ik:addressedTo): in layer `{layer}` only an addressee takes it"
            ),
        });
    }
    Ok(())
}

/// A take of a tuple already taken: a typed Conflict naming who won.
fn already_taken(subject: &str, receipt: &Receipt) -> Error {
    Error::Conflict(format!(
        "<{subject}> was already taken by <{}> at {} (receipt <{}>)",
        receipt.taker, receipt.time, receipt.iri
    ))
}

/// The tuple a take names: `tuple=`, or the piped or trailing value, which the engine puts
/// in `content` for a Delete (`delete urn:layer:tasks:tuples <subject>`). Empty `content`
/// means not given (an engine `delete` always carries it), and both naming different tuples
/// is refused rather than resolved by picking one.
fn take_subject(inv: &Invocation<'_>) -> Result<String> {
    let named = optional_str(inv, "tuple")?
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let piped = match inv.request.args.get("content") {
        None => None,
        Some(_) => Some(
            std::str::from_utf8(inv.inline_arg("content")?)
                .map_err(|_| Error::InvalidArgument {
                    name: "content".into(),
                    detail: "a tuple's subject is not valid UTF-8".into(),
                })?
                .trim(),
        )
        .filter(|s| !s.is_empty()),
    };
    match (named, piped) {
        (Some(a), Some(b)) if a != b => Err(Error::InvalidArgument {
            name: "tuple".into(),
            detail: format!("tuple=<{a}> and the piped <{b}> name different tuples"),
        }),
        (Some(subject), _) | (None, Some(subject)) => Ok(subject.to_string()),
        (None, None) => Err(Error::MissingArgument("tuple".into())),
    }
}

#[async_trait]
impl Endpoint for LayerEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        match self.action {
            Action::Definition => Ok(self.definition()),
            Action::Shapes => Ok(turtle(self.def().shapes.clone()).cacheable()),
            Action::Context => Ok(Representation::new(
                ReprType::new(JSON_LD),
                self.def().context.clone().into_bytes(),
            )
            .cacheable()),
            Action::Feed => self.read_feed(inv).await,
            Action::Say => self.say(inv).await,
            Action::Entries => self.entries(inv).await,
            Action::Entry => self.entry(inv).await,
            Action::Graph => self.graph(inv).await,
            Action::Latest => self.latest(inv).await,
            Action::Tuples => match inv.request.verb {
                Verb::Sink => self.out(inv).await,
                Verb::Source => self.rd(inv).await,
                Verb::Delete => self.take(inv).await,
                other => Err(Error::Endpoint(format!(
                    "a layer's tuples answer Sink (out), Source (rd) and Delete (take), not \
                     {other:?}"
                ))),
            },
            Action::Receipt => self.receipt(inv).await,
        }
    }

    fn name(&self) -> &str {
        self.action.word()
    }

    fn describe(&self) -> Description {
        let layer = self.layer();
        let cap = layer_cap(self.action.cap_verb(), layer);
        let id = endpoint_id(layer, self.action.word());
        let base = Description::new(id).verb(Verb::Meta).requires(cap);
        match self.action {
            Action::Definition => base
                .title(format!("Layer {layer}"))
                .summary(format!(
                    "Layer `{layer}`'s definition: an ik:Layer with its access model and the \
                     IRIs of its shapes, context and feed"
                ))
                .verb(Verb::Source)
                .output(TURTLE),
            Action::Shapes => base
                .title(format!("Layer {layer}: shapes"))
                .summary(format!(
                    "The closed SHACL shapes every entry of layer `{layer}` must conform to"
                ))
                .verb(Verb::Source)
                .output(TURTLE),
            Action::Context => base
                .title(format!("Layer {layer}: context"))
                .summary(format!(
                    "Layer `{layer}`'s egress context: the JSON-LD allowlist of terms that may \
                     cross into it (no @vocab)"
                ))
                .verb(Verb::Source)
                .output(JSON_LD),
            Action::Feed => base
                .title(format!("Layer {layer}: feed"))
                .summary(format!(
                    "Layer `{layer}`'s events after a sequence number (gapless, from 1): one \
                     line per event, `seq time kind iri by`, waiting up to wait= seconds when \
                     there is none yet"
                ))
                .verb(Verb::Source)
                .input(after_spec(
                    "the last sequence number already seen (0 for everything)",
                ))
                .input(wait_spec())
                .input(as_spec(&[TEXT, JSON]))
                .output(TEXT)
                .output(JSON),
            Action::Say => base
                .title(format!("Layer {layer}: say"))
                .summary(format!(
                    "Append one entry to layer `{layer}`, in the caller's own graph: Turtle or \
                     JSON-LD about <urn:layer:new> (or one blank node). The host names it, \
                     stamps who, when and its feed position, admits it only if it conforms to \
                     the layer's closed shapes, and answers its IRI. Append-only: correct an \
                     entry with a new one that ik:supersedes it"
                ))
                .verb(Verb::Sink)
                .input(
                    ArgSpec::new("content")
                        .summary(
                            "the entry, as Turtle or JSON-LD, about <urn:layer:new>; never \
                             prov:wasAttributedTo, dcterms:created or ik:seq, which the host \
                             stamps",
                        )
                        .class(xsd("string")),
                )
                .input(as_spec(&[TEXT, JSON]))
                .output(TEXT)
                .output(JSON),
            Action::Entries => base
                .title(format!("Layer {layer}: entries"))
                .summary(format!(
                    "Layer `{layer}`'s entries in feed order, after a sequence number, \
                     optionally from one participant or of one class"
                ))
                .verb(Verb::Source)
                .input(
                    after_spec("answer entries after this ik:seq (default 0)").default_value("0"),
                )
                .input(
                    ArgSpec::new("from")
                        .summary("only the entries this participant said, by its IRI")
                        .class(xsd("anyURI"))
                        .optional(),
                )
                .input(
                    ArgSpec::new("type")
                        .summary("only the entries of this class")
                        .class(xsd("anyURI"))
                        .optional(),
                )
                .input(limit_spec())
                .input(as_spec(&GRAPH_FACES))
                .output(TURTLE)
                .output(JSON_LD),
            Action::Entry => base
                .title(format!("Layer {layer}: entry"))
                .summary(format!(
                    "One entry of layer `{layer}`, by id; an entry never changes"
                ))
                .verb(Verb::Source)
                .input(
                    ArgSpec::new("id")
                        .summary("the entry's id, the last segment of its IRI")
                        .class(xsd("string"))
                        .binding(),
                )
                .input(as_spec(&GRAPH_FACES))
                .output(TURTLE)
                .output(JSON_LD),
            Action::Graph => base
                .title(format!("Layer {layer}: author graph"))
                .summary(format!(
                    "One participant's entries in layer `{layer}`, in feed order"
                ))
                .verb(Verb::Source)
                .input(
                    ArgSpec::new("slug")
                        .summary(
                            "the author's graph name: a participant's short name, or \
                             client.{host}.{fingerprint} for a host client",
                        )
                        .class(xsd("string"))
                        .binding(),
                )
                .input(
                    after_spec("answer entries after this ik:seq (default 0)").default_value("0"),
                )
                .input(limit_spec())
                .input(as_spec(&GRAPH_FACES))
                .output(TURTLE)
                .output(JSON_LD),
            Action::Latest => base
                .title(format!("Layer {layer}: latest"))
                .summary(format!(
                    "The newest entry of layer `{layer}` about an IRI (by <{}>) that no entry \
                     supersedes",
                    self.def().about
                ))
                .verb(Verb::Source)
                .input(
                    ArgSpec::new("about")
                        .summary("the IRI the entry is about")
                        .class(xsd("anyURI")),
                )
                .input(as_spec(&GRAPH_FACES))
                .output(TURTLE)
                .output(JSON_LD),
            Action::Tuples => {
                let addressed = if self.def().addressed {
                    " This layer is addressed: a tuple carrying ik:addressedTo is taken only by \
                     a participant it names."
                } else {
                    ""
                };
                // `match=` is declared only where the definition allows it (`match = true`), so
                // a layer that refuses it does not offer it (ledger #1327).
                let mut selectors = vec![ArgSpec::new("tuple")
                    .summary("read this tuple (or, with state=taken, its receipt), by its subject")
                    .class(xsd("anyURI"))
                    .optional()];
                let matching = if self.def().matching {
                    selectors.push(
                        ArgSpec::new("match")
                            .summary(
                                "a SPARQL ASK (no SERVICE): list only the pending tuples whose \
                                 graph satisfies it",
                            )
                            .class(xsd("string"))
                            .optional(),
                    );
                    ", those a match= ASK holds for"
                } else {
                    ""
                };
                base.title(format!("Layer {layer}: tuples"))
                    .summary(format!(
                        "Layer `{layer}`'s tuplespace: out (Sink) drops a tuple named by its one \
                         subject under urn:layer:{layer}:, rd (Source) lists or reads the \
                         pending tuples or, with state=taken, the receipts, and take (Delete) \
                         removes one atomically: exactly one taker wins, and its receipt names \
                         it.{addressed}"
                    ))
                    .action(
                        selectors
                            .into_iter()
                            .fold(
                                ActionSpec::new(Verb::Source).summary(format!(
                                    "rd: the pending tuples in the order they were dropped (one \
                                     line each, `seq time subject dropper`; as=application/json \
                                     the same list as JSON; as=text/turtle or \
                                     application/ld+json the tuples themselves), one by \
                                     tuple={matching}; with state=taken, the receipts instead"
                                )),
                                ActionSpec::input,
                            )
                            .input(
                                ArgSpec::new("state")
                                    .summary(
                                        "pending (the space) or taken (the receipts: who took \
                                         each tuple, and when)",
                                    )
                                    .class(xsd("string"))
                                    .one_of(["pending", "taken"])
                                    .default_value("pending"),
                            )
                            .input(
                                after_spec(
                                    "answer tuples dropped (receipts taken) after this ik:seq \
                                     (default 0)",
                                )
                                .default_value("0"),
                            )
                            .input(
                                ArgSpec::new("limit")
                                    .summary(format!(
                                        "the most tuples or receipts to answer, at most \
                                         {MAX_LIMIT}; page with after="
                                    ))
                                    .class(xsd("nonNegativeInteger"))
                                    .default_value(DEFAULT_LIMIT.to_string()),
                            )
                            .input(as_spec(&RD_FACES))
                            .output(TEXT)
                            .output(TURTLE)
                            .output(JSON)
                            .output(JSON_LD)
                            .requires(layer_cap("read", layer)),
                    )
                    .action(
                        ActionSpec::new(Verb::Sink)
                            .summary(format!(
                                "out: drop one tuple, Turtle or JSON-LD about one subject the \
                                 dropper mints under urn:layer:{layer}: (e.g. \
                                 urn:layer:{layer}:offer:{{ulid}}) and never used before here; \
                                 the host stamps who, when and its feed position, admits it \
                                 only if it conforms to the layer's closed shapes, and answers \
                                 its subject. The same drop sent again is one record"
                            ))
                            .input(
                                ArgSpec::new("content")
                                    .summary(
                                        "the tuple, as Turtle or JSON-LD; never \
                                         prov:wasAttributedTo, dcterms:created or ik:seq, \
                                         which the host stamps",
                                    )
                                    .class(xsd("string")),
                            )
                            .input(as_spec(&[TEXT, JSON]))
                            .output(TEXT)
                            .output(JSON)
                            .requires(layer_cap("out", layer)),
                    )
                    .action(
                        ActionSpec::new(Verb::Delete)
                            .summary(
                                "take: remove one pending tuple by its subject, atomically, and \
                                 answer it; a tuple already taken is a Conflict naming its \
                                 taker. The receipt is written before the reply, so a taker \
                                 that lost it reads rd state=taken tuple=<subject>",
                            )
                            .input(
                                ArgSpec::new("tuple")
                                    .summary("the subject of the tuple to take")
                                    .class(xsd("anyURI"))
                                    .optional(),
                            )
                            .input(
                                ArgSpec::new("content")
                                    .summary(
                                        "the subject to take, as the piped or trailing value \
                                         (the same as tuple=; empty = not given)",
                                    )
                                    .class(xsd("string"))
                                    .optional(),
                            )
                            .input(as_spec(&GRAPH_FACES))
                            .output(TURTLE)
                            .output(JSON_LD)
                            .requires(layer_cap("take", layer)),
                    )
            }
            Action::Receipt => base
                .title(format!("Layer {layer}: receipt"))
                .summary(format!(
                    "One take in layer `{layer}`, by id: who took which tuple, when, and the \
                     tuple as taken; a receipt never changes"
                ))
                .verb(Verb::Source)
                .input(
                    ArgSpec::new("id")
                        .summary("the receipt's id, the last segment of its IRI")
                        .class(xsd("string"))
                        .binding(),
                )
                .input(as_spec(&GRAPH_FACES))
                .output(TURTLE)
                .output(JSON_LD),
        }
    }
}

/// `urn:layers:feed`: one wait over several layers' feeds, with a cursor vector.
struct MultiFeed {
    host: Arc<Host>,
}

#[async_trait]
impl Endpoint for MultiFeed {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let cursor = feed::parse_cursor(inv.inline_str("after")?).map_err(|detail| {
            Error::InvalidArgument {
                name: "after".into(),
                detail,
            }
        })?;
        let wait = wait_of(inv)?;
        let as_json = face(inv, &[TEXT, JSON])? == JSON;
        let mut layers: BTreeMap<&str, (&Arc<LayerState>, u64)> = BTreeMap::new();
        for (name, after) in &cursor {
            // An unknown layer and an unheld one are refused alike, so the refusal does
            // not say which layers exist.
            let state = self
                .host
                .layer(name)
                .filter(|_| inv.capability.allows(&layer_cap("feed", name)))
                .ok_or_else(|| {
                    Error::Denied(format!(
                        "no feed grant on layer `{name}` (needs {})",
                        layer_cap("feed", name)
                    ))
                })?;
            let last = state.last_seq();
            if *after > last {
                return Err(Error::Conflict(format!(
                    "cursor {name}:{after} is past layer `{name}`'s last event, {last}: \
                     resynchronize that layer from 0"
                )));
            }
            layers.insert(name.as_str(), (state, *after));
        }
        feed::wait_until(self.host.notify(), wait, || {
            layers
                .values()
                .any(|(state, after)| state.last_seq() > *after)
        })
        .await;
        let mut events = Vec::new();
        let mut next = cursor.clone();
        for (name, (state, after)) in &layers {
            for event in state.events_after(*after) {
                next.insert(name.to_string(), event.seq);
                events.push((name.to_string(), event));
            }
        }
        let repr = if as_json {
            let events: Vec<serde_json::Value> = events
                .iter()
                .map(|(layer, e)| {
                    serde_json::json!({
                        "layer": layer, "seq": e.seq, "time": e.time, "kind": e.kind,
                        "iri": e.iri, "by": e.by,
                    })
                })
                .collect();
            let body = serde_json::json!({
                "cursor": feed::format_cursor(&next),
                "events": events,
            });
            Representation::new(ReprType::new(JSON), body.to_string().into_bytes())
        } else {
            let text: String = events
                .iter()
                .map(|(layer, e)| {
                    format!(
                        "{layer} {} {} {} {} {}\n",
                        e.seq, e.time, e.kind, e.iri, e.by
                    )
                })
                .collect();
            Representation::new(ReprType::new(TEXT), text.into_bytes())
        };
        Ok(repr)
    }

    fn name(&self) -> &str {
        "layers-feed"
    }

    fn describe(&self) -> Description {
        Description::new("layers-feed")
            .title("Layer feeds")
            .summary(
                "One wait over several layers' feeds: events after a cursor vector \
                 (after=talk:12,tasks:40), one line per event prefixed with its layer, waiting \
                 up to wait= seconds on any of them. Each layer named needs its own exact \
                 urn:cap:layer:feed:{layer}; the JSON face carries the next cursor",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_FEED_FAMILY)
            .input(
                ArgSpec::new("after")
                    .summary("the cursor: layer:seq pairs, comma-separated")
                    .class(xsd("string")),
            )
            .input(wait_spec())
            .input(as_spec(&[TEXT, JSON]))
            .output(TEXT)
            .output(JSON)
    }
}

/// `urn:layers:participants`: who each principal is.
struct ParticipantsEndpoint {
    host: Arc<Host>,
}

#[async_trait]
impl Endpoint for ParticipantsEndpoint {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        let participants = self.host.participants().map_err(host_error)?;
        // Read from the file on every request (enrollment is live), so never cached.
        Ok(turtle(rdf::to_turtle(&participants.triples)))
    }

    fn name(&self) -> &str {
        "layers-participants"
    }

    fn describe(&self) -> Description {
        Description::new("layers-participants")
            .title("Layer participants")
            .summary(
                "The participants graph: who each principal this host's layers attribute is \
                 (a person, an agent acting for one, or a machine)",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_PARTICIPANTS)
            .output(TURTLE)
    }
}

/// `urn:layers:me`: the caller's principal as its door minted it and, when that principal is
/// a participant, who the participants graph says it is and the collaborations whose
/// manifests grant it anything (ledger #1314).
///
/// A client otherwise has no way to learn which principal its door minted for it: gonk's
/// mirror and taker inferred it from the attribution on records they already held, which
/// cannot resolve a take whose reply was lost before the first take (ledger #1313).
///
/// **The capability.** It declares `urn:cap:collab:read:*`, the family every generated
/// grant already holds (each grant carries `urn:cap:collab:read:{name}` for every manifest
/// naming its participant), so no grant needs a new token. A collaboration is listed only
/// when the caller also holds its exact token: what an attenuated capability is answered
/// never exceeds what it could read at `urn:collab:{name}`. Under a generated grant the two
/// lists are the same.
///
/// **No principal is a refusal** (`Denied`), never an empty answer: a door that mints none
/// leaves its caller unable to write anything (every write is attributed), an empty Turtle
/// graph cannot be told from a broken read, and a capability that carries a principal would
/// succeed. Root names nobody, so root is refused too.
struct MeEndpoint {
    host: Arc<Host>,
}

/// One word for what a participant is, from its `rdf:type`s: a machine (`ik:Machine`,
/// itself a `prov:SoftwareAgent`, so checked first), a person (`foaf:Person`), an agent
/// (`prov:SoftwareAgent`), or `other`.
fn kind_of(types: &std::collections::BTreeSet<String>) -> &'static str {
    if types.contains(&rdf::ik_machine()) {
        "machine"
    } else if types.contains(rdf::FOAF_PERSON) {
        "person"
    } else if types.contains(rdf::PROV_SOFTWARE_AGENT) {
        "agent"
    } else {
        "other"
    }
}

#[async_trait]
impl Endpoint for MeEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let principal = inv.capability.principal().ok_or_else(|| {
            Error::Denied(
                "urn:layers:me answers the principal the door minted for the caller, and this \
                 request names nobody (root names nobody either)"
                    .into(),
            )
        })?;
        let as_json = face(inv, &[TURTLE, JSON])? == JSON;
        // Read from the files on every request (enrollment is live), so never cached.
        let participants = self.host.participants().map_err(host_error)?;
        let member = participants.members.get(principal);
        let collabs: Vec<String> = match member {
            None => Vec::new(),
            Some(_) => self
                .host
                .collabs_with(&participants)
                .map_err(host_error)?
                .values()
                .filter(|m| {
                    m.mounts
                        .iter()
                        .any(|mount| mount.grants.keys().any(|p| p.iri() == principal))
                })
                .filter(|m| inv.capability.allows(&collab_cap(&m.name)))
                .map(|m| collab_iri(&m.name))
                .collect(),
        };
        let about: Vec<&Triple> = participants
            .triples
            .iter()
            .filter(|t| rdf::subject_iri(&t.subject) == Some(principal))
            .collect();
        if as_json {
            let acts_for: Vec<&str> = about
                .iter()
                .filter(|t| t.predicate.as_str() == rdf::PROV_ACTED_FOR)
                .filter_map(|t| rdf::object_iri(&t.object))
                .collect();
            let body = serde_json::json!({
                "principal": principal,
                "participant": member.map(|_| principal),
                "kind": member.map(|m| kind_of(&m.types)),
                "types": member.map(|m| m.types.iter().collect::<Vec<_>>()).unwrap_or_default(),
                "label": member.and_then(|m| m.label.as_deref()),
                "actsFor": acts_for,
                "collaborations": collabs,
            });
            return Ok(Representation::new(
                ReprType::new(JSON),
                body.to_string().into_bytes(),
            ));
        }
        let me = node(principal);
        let mut triples = vec![Triple::new(
            node(ME),
            node(rdf::FOAF_PRIMARY_TOPIC),
            me.clone(),
        )];
        triples.extend(about.into_iter().cloned());
        for collab in &collabs {
            let c = node(collab);
            triples.push(Triple::new(
                c.clone(),
                node(rdf::RDF_TYPE),
                node(&rdf::ik_collaboration()),
            ));
            triples.push(Triple::new(c, node(rdf::DCT_CONTRIBUTOR), me.clone()));
        }
        Ok(turtle(rdf::to_turtle(&triples)))
    }

    fn name(&self) -> &str {
        "layers-me"
    }

    fn describe(&self) -> Description {
        Description::new("layers-me")
            .title("Who am I")
            .summary(
                "The caller's principal, as the door that authenticated it minted it (the \
                 subject of urn:layers:me's foaf:primaryTopic), and when it is a participant, \
                 what the participants graph says of it and each collaboration whose manifest \
                 grants it anything (dcterms:contributor); the JSON face adds its kind \
                 (person, agent, machine or other). Readable under any participant's grant; \
                 a request that names no principal is refused",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_COLLAB_FAMILY)
            .input(as_spec(&[TURTLE, JSON]))
            .output(TURTLE)
            .output(JSON)
    }
}

/// `urn:collab:{name}:participants`: who takes part in one collaboration, the participants
/// graph restricted to the principals its manifest grants anything (ledger #1282).
///
/// The participants graph names every principal the host attributes, across every
/// collaboration, so it is the operator's ([`CAP_PARTICIPANTS`]). This is the slice a
/// participant may read: the same triples, about exactly the principals its own
/// collaboration's manifest already names as contributors, under the manifest's own token.
/// A mirror reads each participant's kind here, and an agent reads for whom it acts.
struct CollabParticipants {
    host: Arc<Host>,
}

#[async_trait]
impl Endpoint for CollabParticipants {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = inv.bindings.get("name").unwrap_or_default();
        // As `urn:collab:{name}`: the token is checked before anything is read, so an
        // absent collaboration is refused like an unheld one and the refusal names nothing.
        if !inv.capability.allows(&collab_cap(name)) {
            return Err(Error::Denied(format!(
                "reading collaboration `{name}`'s participants needs {}",
                collab_cap(name)
            )));
        }
        let participants = self.host.participants().map_err(host_error)?;
        let collabs = self.host.collabs_with(&participants).map_err(host_error)?;
        let manifest = collabs
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("no collaboration `{name}`")))?;
        let members: std::collections::BTreeSet<&str> = manifest
            .mounts
            .iter()
            .flat_map(|m| m.grants.keys().map(Participant::iri))
            .collect();
        let slice: Vec<Triple> = participants
            .triples
            .iter()
            .filter(|t| match &t.subject {
                oxrdf::NamedOrBlankNode::NamedNode(n) => members.contains(n.as_str()),
                _ => false,
            })
            .cloned()
            .collect();
        // Read from the files on every request (enrollment is live), so never cached.
        Ok(turtle(rdf::to_turtle(&slice)))
    }

    fn name(&self) -> &str {
        "collab-participants"
    }

    fn describe(&self) -> Description {
        Description::new("collab-participants")
            .title("Collaboration participants")
            .summary(
                "Who takes part in one collaboration: each participant its manifest grants \
                 anything, as the participants graph describes it (a person, an agent and for \
                 whom it acts, or a machine). Needs the manifest's own \
                 urn:cap:collab:read:{name}",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_COLLAB_FAMILY)
            .input(
                ArgSpec::new("name")
                    .summary("the collaboration's name")
                    .class(xsd("string"))
                    .binding(),
            )
            .output(TURTLE)
    }
}

/// `urn:collab:{name}`: a collaboration's manifest.
struct CollabEndpoint {
    host: Arc<Host>,
}

impl CollabEndpoint {
    fn turtle(manifest: &Manifest) -> String {
        let s = node(&collab_iri(&manifest.name));
        let mut triples = vec![
            Triple::new(
                s.clone(),
                node(rdf::RDF_TYPE),
                node(&rdf::ik_collaboration()),
            ),
            Triple::new(
                s.clone(),
                node(rdf::RDFS_LABEL),
                Literal::new_simple_literal(manifest.label.as_deref().unwrap_or(&manifest.name)),
            ),
        ];
        let mut contributors = std::collections::BTreeSet::new();
        for mount in &manifest.mounts {
            triples.push(Triple::new(
                s.clone(),
                node(rdf::DCT_HAS_PART),
                node(&layer_iri(&mount.layer)),
            ));
            contributors.extend(mount.grants.keys().map(|p| p.iri().to_string()));
        }
        for p in contributors {
            triples.push(Triple::new(s.clone(), node(rdf::DCT_CONTRIBUTOR), node(&p)));
        }
        rdf::to_turtle(&triples)
    }

    fn json(manifest: &Manifest) -> String {
        let mounts: Vec<serde_json::Value> = manifest
            .mounts
            .iter()
            .map(|m| {
                let grants: BTreeMap<&str, Vec<&str>> = m
                    .grants
                    .iter()
                    .map(|(p, verbs)| (p.iri(), verbs.iter().map(String::as_str).collect()))
                    .collect();
                serde_json::json!({ "layer": m.layer, "grants": grants })
            })
            .collect();
        serde_json::json!({
            "collaboration": collab_iri(&manifest.name),
            "label": manifest.label,
            "mounts": mounts,
        })
        .to_string()
    }
}

#[async_trait]
impl Endpoint for CollabEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = inv.bindings.get("name").unwrap_or_default();
        // The family is declared; the manifest's own token is checked here, and an absent
        // manifest is refused like an unheld one, so the refusal names nothing.
        if !inv.capability.allows(&collab_cap(name)) {
            return Err(Error::Denied(format!(
                "reading collaboration `{name}` needs {}",
                collab_cap(name)
            )));
        }
        let collabs = self.host.collabs().map_err(host_error)?;
        let manifest = collabs
            .get(name)
            .ok_or_else(|| Error::NotFound(format!("no collaboration `{name}`")))?;
        let repr = if face(inv, &[TURTLE, JSON])? == JSON {
            Representation::new(ReprType::new(JSON), Self::json(manifest).into_bytes())
        } else {
            turtle(Self::turtle(manifest))
        };
        Ok(repr)
    }

    fn name(&self) -> &str {
        "collab"
    }

    fn describe(&self) -> Description {
        Description::new("collab")
            .title("Collaboration")
            .summary(
                "A collaboration's manifest: the layers it mounts (dcterms:hasPart) and who \
                 takes part (dcterms:contributor); the JSON face carries each participant's \
                 verbs on each layer. Needs the manifest's own urn:cap:collab:read:{name}",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .requires(CAP_COLLAB_FAMILY)
            .input(
                ArgSpec::new("name")
                    .summary("the collaboration's name")
                    .class(xsd("string"))
                    .binding(),
            )
            .input(as_spec(&[TURTLE, JSON]))
            .output(TURTLE)
            .output(JSON)
    }
}
