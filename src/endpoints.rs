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
//! urn:layers:feed                  Source  layers-feed               urn:cap:layer:feed:*  (+ exact per layer)
//! urn:layers:participants          Source  layers-participants       urn:cap:layers:read:participants
//! urn:collab:{name}                Source  collab                    urn:cap:collab:read:* (+ exact per manifest)
//! ```
//!
//! **Why exact, and why per layer.** Core's selection matches a declared scope EXACTLY
//! unless it ends in `*`, and then it prefix-tests what is held after stripping the `*`
//! (`cap_satisfies`). So `urn:cap:layer:say:talk` is satisfied by that token alone (or root):
//! not by `urn:cap:layer:say:talk-bobby`, and not by a held `urn:cap:layer:say:*` either,
//! which is held as a literal string and allows only itself. Binding one endpoint per
//! layer is what lets each declare its own exact token, so the manifold under a
//! participant's grant, and the MCP tool list projected from it, is exactly the layers it
//! holds. The two host-wide reads that span names (`urn:layers:feed`, `urn:collab:{name}`)
//! must declare the family and check the exact token inside; they offer themselves to
//! anyone holding some grant of the family, and refuse the rest by name.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ikigai_core::{
    ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Request, Result, UriTemplate, Verb,
};
use oxrdf::{Literal, Triple};

use crate::channel::{self, entry_prefix};
use crate::config::{ConfigError, LayerDef, Manifest};
use crate::feed::{self, Event, MAX_WAIT_SECS};
use crate::host::{Host, LayerState};
use crate::id;
use crate::name::{collab_cap, collab_iri, endpoint_id, layer_cap, layer_iri, Participant};
use crate::rdf::{self, node};
use crate::time::xsd_date_time;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const TURTLE: &str = "text/turtle";
const JSON: &str = "application/json";
const JSON_LD: &str = "application/ld+json";
const TEXT: &str = "text/plain";

/// The validator `say` composes for admission.
pub const VALIDATE: &str = "urn:shacl:validate";

/// The capability that reads the participants graph. Not generated for anyone: who takes
/// part in OTHER collaborations is not a participant's business (isolation), so this is an
/// operator's grant (and a mirror's, which needs a participant's kind).
pub const CAP_PARTICIPANTS: &str = "urn:cap:layers:read:participants";

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
        for action in Action::ALL {
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
}

impl Action {
    const ALL: [Action; 9] = [
        Action::Definition,
        Action::Shapes,
        Action::Context,
        Action::Feed,
        Action::Say,
        Action::Entries,
        Action::Entry,
        Action::Graph,
        Action::Latest,
    ];

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
        }
    }

    /// The template variable this action's IRI captures, if any.
    fn binding(self) -> Option<&'static str> {
        match self {
            Action::Entry => Some("id"),
            Action::Graph => Some("slug"),
            _ => None,
        }
    }

    fn iri(self, base: &str) -> String {
        match self {
            Action::Definition => base.to_string(),
            Action::Entry => format!("{base}:entry:{{id}}"),
            Action::Graph => format!("{base}:graph:{{slug}}"),
            other => format!("{base}:{}", other.word()),
        }
    }

    /// The `{verb}` of the capability this action requires.
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
        for part in ["shapes", "context", "feed"] {
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

    async fn say(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let author = inv
            .capability
            .principal()
            .ok_or_else(|| {
                Error::Denied(format!(
                    "`say` on layer `{}` needs a principal, minted by the door that \
                     authenticated the caller: every entry is attributed, and this request \
                     names nobody (root names nobody either)",
                    self.layer()
                ))
            })?
            .to_string();
        let participants = self.host.participants().map_err(host_error)?;
        if !participants.names(&author) {
            return Err(Error::Denied(format!(
                "{author} is not a participant of this host (participants.ttl does not name it)"
            )));
        }
        let draft = channel::draft(self.def(), inv.inline_arg("content")?)?;
        // From here to the feed append is one step per layer: the sequence number is
        // assigned, stamped, admitted, stored and logged under this lock.
        let _write = self.state.write.lock().await;
        self.state
            .channel()
            .read()
            .expect("channel lock")
            .check_supersedes(&draft, &author)?;
        let now = inv.now().ok_or_else(|| {
            Error::Endpoint(
                "this host gives the kernel no clock, and every entry is stamped with its time"
                    .into(),
            )
        })?;
        let seq = self.state.last_seq() + 1;
        let iri = format!(
            "{}{}",
            entry_prefix(self.layer()),
            id::mint(now.as_millis(), seq)
        );
        let created = xsd_date_time(now);
        let stamped = draft.stamp(&iri, &author, &created, seq);
        admit(inv, self.def(), &stamped).await?;
        self.state
            .channel()
            .write()
            .expect("channel lock")
            .append(&stamped)
            .map_err(Error::Endpoint)?;
        self.state
            .feed
            .lock()
            .expect("feed lock")
            .append(Event {
                seq,
                time: created,
                kind: "say".into(),
                iri: iri.clone(),
                by: author,
            })
            .map_err(Error::Endpoint)?;
        drop(_write);
        self.host.notify().bump();
        Ok(Representation::new(ReprType::new(TEXT), iri.into_bytes()))
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
    /// own Turtle document, and Turtle documents concatenate).
    fn entries_where(
        &self,
        inv: &Invocation<'_>,
        keep: impl Fn(&channel::EntryMeta) -> bool,
    ) -> Result<Representation> {
        let (after, limit) = self.page(inv)?;
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
        Ok(turtle(text).cacheable().depends_on(self.say_thread()))
    }

    fn entries(&self, inv: &Invocation<'_>) -> Result<Representation> {
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
    }

    fn entry(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let id = inv.bindings.get("id").unwrap_or_default();
        let channel = self.state.channel().read().expect("channel lock");
        match channel.get(id) {
            // An entry never changes: cacheable for as long as the layer exists.
            Some(meta) => Ok(turtle(channel.turtle(meta).map_err(Error::Endpoint)?).cacheable()),
            None => {
                Err(self.not_found(inv, format!("no entry `{id}` in layer `{}`", self.layer())))
            }
        }
    }

    fn graph(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let slug = inv.bindings.get("slug").unwrap_or_default().to_string();
        self.entries_where(inv, |m| m.slug == slug)
    }

    fn latest(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let about = inv.inline_str("about")?;
        let channel = self.state.channel().read().expect("channel lock");
        match channel.latest(about) {
            Some(meta) => Ok(turtle(channel.turtle(meta).map_err(Error::Endpoint)?)
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
}

/// Admission: the stamped entry must conform to the layer's closed shapes, by a
/// sub-request to `urn:shacl:validate` (composed, not linked).
///
/// - It answers `Ok` for data that does NOT conform (the report says `conforms: false`),
///   so admission reads the report's `conforms`, never the `Ok`.
/// - The shapes are passed INLINE, from the definition the host holds, so a writer needs no
///   read grant on the shapes, and the check runs under an EMPTY capability: validation
///   needs no authority, and the writer's is not lent to it.
/// - Any failure of the validator itself (no validator bound, the operator's shapes do not
///   compile) is the HOST's error, never the writer's bad argument, and nothing is admitted.
async fn admit(inv: &Invocation<'_>, def: &LayerDef, stamped: &[Triple]) -> Result<()> {
    let request = Request::new(Verb::Source, Iri::parse(VALIDATE).expect("a constant IRI"))
        .with_arg("data", ArgRef::Inline(rdf::to_turtle(stamped).into_bytes()))
        .with_arg("shapes", ArgRef::Inline(def.shapes.clone().into_bytes()))
        .with_arg("as", ArgRef::Inline(JSON.as_bytes().to_vec()));
    let report = match inv.issue_attenuated(request, Vec::<String>::new()).await {
        Ok(report) => report,
        Err(Error::Unresolved(_)) => {
            return Err(Error::Endpoint(format!(
                "layer `{}`: admission composes {VALIDATE}, which this host does not bind; \
                 nothing is admitted without it",
                def.name
            )))
        }
        Err(e) => {
            return Err(Error::Endpoint(format!(
                "layer `{}`: admission could not run ({e}); the shapes are the operator's",
                def.name
            )))
        }
    };
    let report: serde_json::Value = serde_json::from_slice(&report.bytes).map_err(|e| {
        Error::Endpoint(format!(
            "{VALIDATE} answered a report that is not JSON: {e}"
        ))
    })?;
    match report.get("conforms").and_then(serde_json::Value::as_bool) {
        Some(true) => Ok(()),
        Some(false) => Err(Error::InvalidArgument {
            name: "content".into(),
            detail: violations(&def.name, &report),
        }),
        None => Err(Error::Endpoint(format!(
            "{VALIDATE} answered a report with no `conforms`"
        ))),
    }
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
            Action::Entries => self.entries(inv),
            Action::Entry => self.entry(inv),
            Action::Graph => self.graph(inv),
            Action::Latest => self.latest(inv),
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
                .output(TEXT),
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
                .output(TURTLE),
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
                .output(TURTLE),
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
                .output(TURTLE),
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
                .output(TURTLE),
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
