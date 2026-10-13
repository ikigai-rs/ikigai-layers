//! **A gateway between kernels**, through layers (ledger #1370): one kernel asks another to
//! resolve a resource and gets the answer back, with neither resolving into the other.
//!
//! The protocol is CONFIGURATION, not code: an ask layer per asker (a tuplespace, `addressed`),
//! an event channel per resource owner, closed shapes ([`SHAPES`], [`EVENT_SHAPES`]) and egress
//! contexts ([`CONTEXT`], [`EVENT_CONTEXT`]); the example configuration ships in the crate's
//! `gateway/` directory. This module is the reference PARTICIPANT on both ends, for an ikigai
//! kernel: an [`Asker`] drops a `gw:Request` and waits for its answer with a deadline, and a
//! [`Resolver`] takes requests ([`serve`], [`serve_pending`], [`follow`]), resolves each in its
//! OWN kernel and drops a `gw:Reply` or a `gw:Failure`.
//!
//! ```text
//! urn:layer:{layer}:request:{correlation}   gw:Request  the asker's; the host stamps who asked
//! urn:layer:{layer}:reply:{correlation}     gw:Reply    the representation, base64 + media type
//!                                           gw:Failure  or a typed error: Error::kind()'s words
//! ```
//!
//! # What the host enforces, so this module does not have to
//!
//! A request is one tuple, so the atomic take gives it to exactly ONE resolver; the other is
//! answered with a Conflict naming the winner. A reply names its request with `ik:offer` and is
//! addressed (`ik:addressedTo`) to the asker: the host admits it only from the participant whose
//! take of that request it holds a receipt for, and in an `addressed` layer only when it is
//! addressed to the asker alone, and only the asker can take it. Who asked is the host's stamp
//! (`prov:wasAttributedTo`), never a field an asker writes.
//!
//! # Whose authority a request runs under
//!
//! **The resolver's, never the asker's.** No capability crosses a layer: a request carries a
//! name, a verb and arguments, and the resolver issues it into its own kernel through its own
//! [`Door`], under the capability its operator gave that door. That is bounded three ways: the
//! door's capability (what any request can reach at all), the resolver's VERBS (by default only
//! the three that cannot change anything: Source, Exists, Meta; a mutating verb is refused as
//! Denied unless the operator allows it) and its PREFIXES (when set, a name outside them is
//! refused as Denied before the kernel sees it). Every asker on a layer gets the same authority:
//! the layer grant says who may ask, the resolver says what asking can do.
//!
//! The example runs both ends against a scratch layer host in this process, so it needs the
//! `host` feature; a participant built without it uses exactly the same `Asker`, `Resolver` and
//! `follow`, over a [`Door`] onto some other host's door.
//!
//! ```
//! # #[cfg(feature = "host")]
//! # fn main() {
//! use std::sync::Arc;
//! use std::time::Duration;
//! use futures::executor::block_on;
//! use ikigai_core::{Capability, Iri, Kernel, SystemClock, Verb};
//! use ikigai_layers::gateway::{self, Ask, Asker, KernelDoor, Resolver};
//!
//! # let dir = std::env::temp_dir().join(format!("ikigai-layers-gw-doc-{}", std::process::id()));
//! # let (config, state) = (dir.join("config"), dir.join("state"));
//! # let write = |rel: &str, text: &str| {
//! #     let p = config.join(rel);
//! #     std::fs::create_dir_all(p.parent().unwrap()).unwrap();
//! #     std::fs::write(p, text).unwrap();
//! # };
//! # write("layers/gateway.shapes.ttl", gateway::SHAPES);
//! # write("layers/gateway.context.jsonld", gateway::CONTEXT);
//! # write("layers/gateway-a.toml", "model = \"tuplespace\"\nshapes = \"gateway.shapes.ttl\"\ncontext = \"gateway.context.jsonld\"\naddressed = true\n");
//! # write("participants.ttl", "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:collab:participant:a> a ik:Machine .\n<urn:collab:participant:r> a ik:Machine .\n");
//! # write("collabs/gw.toml", "[[mount]]\nlayer = \"gateway-a\"\na = [\"read\", \"feed\", \"out\", \"take\"]\nr = [\"read\", \"feed\", \"out\", \"take\"]\n");
//! let host = ikigai_layers::Host::open(&config, &state).expect("a gateway configuration");
//! let layers = Kernel::new(Arc::new(ikigai_layers::space(&host))).with_clock(Arc::new(SystemClock));
//! let grant = |who: &str| {
//!     Capability::scoped(["read", "feed", "out", "take"].map(|v| format!("urn:cap:layer:{v}:gateway-a")))
//!         .with_principal(&format!("urn:collab:participant:{who}"))
//!         .unwrap()
//! };
//! // The resolver's own kernel: here, the layer host's own kernel stands in for it, and it
//! // resolves the asker's name under the capability ITS operator chose (root, here).
//! let resolver = Resolver::new(KernelDoor::new(&layers, Capability::root()));
//! let asker = Asker::new(KernelDoor::new(&layers, grant("a")), "gateway-a");
//! let resolving = KernelDoor::new(&layers, grant("r"));
//!
//! let ask = Ask::new(Verb::Source, Iri::parse("urn:layer:gateway-a").unwrap());
//! let answer = std::thread::scope(|s| {
//!     s.spawn(|| block_on(gateway::follow(&resolving, "gateway-a", &resolver, 0, Duration::from_secs(5))));
//!     block_on(asker.ask(&ask, Duration::from_secs(10)))
//! });
//! assert_eq!(answer.unwrap().repr_type.media_type, "text/turtle");
//! # drop(host);
//! # std::fs::remove_dir_all(&dir).unwrap();
//! # }
//! # #[cfg(not(feature = "host"))]
//! # fn main() {}
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use futures::future::{select, Either};
use ikigai_core::{
    ArgRef, Capability, Error, Iri, Kernel, ReprType, Representation, Request, Result, Verb,
};
use oxrdf::{Literal, Term, Triple};

use crate::rdf::{self, node, object_iri, subject_iri};

/// The namespace of the gateway's own terms (`gw:`): layer-local, never core's `ik:`.
pub const NS: &str = "urn:layers:gateway:";

/// The closed shapes of an ask layer: `gw:Request`, `gw:Reply`, `gw:Failure`.
pub const SHAPES: &str = include_str!("../gateway/layers/gateway.shapes.ttl");
/// The egress context of an ask layer (no `@vocab`).
///
/// `argument` is the one term a request may carry more than once, so it is
/// `"@container": "@set"`: in the JSON-LD face it is ALWAYS an array, one argument or
/// several, and absent when there are none. Every other term is single-valued.
///
/// ```
/// let context: serde_json::Value =
///     serde_json::from_str(ikigai_layers::gateway::CONTEXT).unwrap();
/// let argument = &context["@context"]["argument"];
/// assert_eq!(argument["@id"], "gw:argument");
/// assert_eq!(argument["@container"], "@set");
/// ```
pub const CONTEXT: &str = include_str!("../gateway/layers/gateway.context.jsonld");
/// The closed shapes of an event channel: `gw:Invalidation`.
pub const EVENT_SHAPES: &str = include_str!("../gateway/layers/gateway-events.shapes.ttl");
/// The egress context of an event channel (no `@vocab`).
pub const EVENT_CONTEXT: &str = include_str!("../gateway/layers/gateway-events.context.jsonld");

/// The closed set a `gw:Failure`'s `gw:errorKind` takes: ikigai-core's `Error::kind()` words,
/// one per variant, so every error a kernel answers crosses typed.
pub const ERROR_KINDS: [&str; 10] = [
    "unresolved",
    "missing-argument",
    "invalid-argument",
    "endpoint",
    "denied",
    "not-found",
    "conflict",
    "timeout",
    "unavailable",
    "depth-exceeded",
];

/// The longest `gw:message` a failure carries (the shape's bound): a longer one is cut, on a
/// character boundary, so an answer is never refused for its own error's length.
pub const MAX_MESSAGE: usize = 4000;

/// The longest `gw:argumentName` (the shape's bound).
pub const MAX_ARGUMENT_NAME: usize = 64;

/// The longest `ik:mediaType` (the shape's bound).
pub const MAX_MEDIA_TYPE: usize = 255;

/// A `gw:` term.
pub fn term(local: &str) -> String {
    format!("{NS}{local}")
}

fn ik(local: &str) -> String {
    format!("{}{local}", rdf::IK)
}

const XSD_BASE64: &str = "http://www.w3.org/2001/XMLSchema#base64Binary";

/// A verb's word, as `ik:verb` spells it (the five of core's `Verb`).
pub fn verb_word(verb: Verb) -> &'static str {
    match verb {
        Verb::Source => "Source",
        Verb::Sink => "Sink",
        Verb::Exists => "Exists",
        Verb::Delete => "Delete",
        Verb::Meta => "Meta",
    }
}

/// The verb an `ik:verb` word names.
pub fn verb_of(word: &str) -> Option<Verb> {
    [
        Verb::Source,
        Verb::Sink,
        Verb::Exists,
        Verb::Delete,
        Verb::Meta,
    ]
    .into_iter()
    .find(|v| verb_word(*v) == word)
}

/// `urn:layer:{layer}:request:{correlation}`: a request's name, which its asker mints.
pub fn request_subject(layer: &str, correlation: &str) -> String {
    format!("urn:layer:{layer}:request:{correlation}")
}

/// `urn:layer:{layer}:reply:{correlation}`: the name of a request's answer (a reply or a
/// failure), which the asker knows before it exists and takes by.
pub fn reply_subject(layer: &str, correlation: &str) -> String {
    format!("urn:layer:{layer}:reply:{correlation}")
}

fn tuples(layer: &str) -> String {
    format!("urn:layer:{layer}:tuples")
}

fn layer_request(verb: Verb, iri: &str, args: &[(&str, &[u8])]) -> Request {
    let mut request = Request::new(
        verb,
        Iri::parse(iri).unwrap_or_else(|e| panic!("`{iri}`: {e:?}")),
    );
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.to_vec()));
    }
    request
}

/// Whether `correlation` is one the shapes admit: 1 to 64 of `[0-9A-Za-z._-]`, starting with a
/// letter or digit. It is part of two IRIs, so it is checked before either is built.
pub fn is_correlation(correlation: &str) -> bool {
    let mut bytes = correlation.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphanumeric())
        && correlation.len() <= 64
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// What an asker asks: a verb on a resource, with named text arguments, optionally addressed to
/// one resolver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ask {
    /// The verb.
    pub verb: Verb,
    /// The resource, in the RESOLVER's kernel.
    pub resource: Iri,
    /// Named arguments, as `name` and its text value, in order.
    pub arguments: Vec<(String, String)>,
    /// The one resolver that may take it, or `None` for whichever takes it first.
    pub resolver: Option<String>,
}

impl Ask {
    /// `verb` on `resource`, with no arguments, for any resolver.
    pub fn new(verb: Verb, resource: Iri) -> Ask {
        Ask {
            verb,
            resource,
            arguments: Vec::new(),
            resolver: None,
        }
    }

    /// With one more argument.
    pub fn arg(mut self, name: impl Into<String>, value: impl Into<String>) -> Ask {
        self.arguments.push((name.into(), value.into()));
        self
    }

    /// Addressed to one resolver (its participant IRI).
    pub fn to(mut self, resolver: impl Into<String>) -> Ask {
        self.resolver = Some(resolver.into());
        self
    }
}

/// A request as the host stored it: the [`Ask`], its correlation id, its name, and who asked
/// (the host's stamp).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asked {
    /// `urn:layer:{layer}:request:{correlation}`.
    pub subject: String,
    /// Its correlation id.
    pub correlation: String,
    /// Who asked: the principal its door minted, as the host stamped it.
    pub asker: String,
    /// What was asked.
    pub ask: Ask,
}

/// A request as its asker drops it, in Turtle: one subject, `gw:Request`, its correlation id,
/// resource, verb and arguments (each `name=value`). The host adds who, when and where.
///
/// ```
/// use ikigai_core::{Iri, Verb};
/// use ikigai_layers::gateway::{request_turtle, Ask};
///
/// let ask = Ask::new(Verb::Source, Iri::parse("urn:data:a").unwrap()).arg("as", "text/plain");
/// let turtle = request_turtle("gateway-a", "c1", &ask).unwrap();
/// let triples = ikigai_layers::rdf::parse_turtle(&turtle).unwrap();
/// let has = |p: &str, o: &str| triples.iter().any(|t| {
///     t.subject.to_string() == "<urn:layer:gateway-a:request:c1>"
///         && t.predicate.as_str() == p
///         && t.object.to_string() == o
/// });
/// assert!(has("http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:layers:gateway:Request>"));
/// assert!(has("urn:layers:gateway:correlation", "\"c1\""));
/// assert!(has("urn:layers:gateway:resource", "<urn:data:a>"));
/// assert!(has("https://ikigai-rs.dev/ns#verb", "\"Source\""));
/// assert!(has("urn:layers:gateway:argument", "\"as=text/plain\""));
/// assert_eq!(triples.len(), 5);
/// ```
///
/// # Errors
///
/// `InvalidArgument` for a correlation id the shapes would refuse, an argument name that is
/// not `[A-Za-z_][A-Za-z0-9_.-]*` of at most 64, or a resolver that is not an IRI: caught here,
/// before anything is sent.
pub fn request_turtle(layer: &str, correlation: &str, ask: &Ask) -> Result<String> {
    if !is_correlation(correlation) {
        return Err(Error::InvalidArgument {
            name: "correlation".into(),
            detail: format!(
                "`{correlation}` is not a correlation id: 1 to 64 of [0-9A-Za-z._-], starting \
                 with a letter or digit"
            ),
        });
    }
    let subject = node(&request_subject(layer, correlation));
    let mut triples = vec![
        Triple::new(subject.clone(), node(rdf::RDF_TYPE), node(&term("Request"))),
        Triple::new(
            subject.clone(),
            node(&term("correlation")),
            Literal::new_simple_literal(correlation),
        ),
        Triple::new(
            subject.clone(),
            node(&term("resource")),
            node(ask.resource.as_str()),
        ),
        Triple::new(
            subject.clone(),
            node(&ik("verb")),
            Literal::new_simple_literal(verb_word(ask.verb)),
        ),
    ];
    for (name, value) in &ask.arguments {
        if !is_argument_name(name) {
            return Err(Error::InvalidArgument {
                name: name.clone(),
                detail: "not an argument name a gateway carries: [A-Za-z_][A-Za-z0-9_.-]*, at \
                         most 64"
                    .into(),
            });
        }
        triples.push(Triple::new(
            subject.clone(),
            node(&term("argument")),
            Literal::new_simple_literal(format!("{name}={value}")),
        ));
    }
    if let Some(resolver) = &ask.resolver {
        let to = oxrdf::NamedNode::new(resolver.as_str()).map_err(|e| Error::InvalidArgument {
            name: "resolver".into(),
            detail: format!("`{resolver}` is not an IRI: {e}"),
        })?;
        triples.push(Triple::new(subject, node(&rdf::ik_addressed_to()), to));
    }
    Ok(rdf::to_turtle_with(&triples, &[("gw", NS)]))
}

fn is_argument_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && name.len() <= MAX_ARGUMENT_NAME
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// The triples about `subject` in `text`, by predicate: each value's term.
fn about(text: &str, subject: &str) -> std::result::Result<BTreeMap<String, Vec<Term>>, String> {
    let mut found: BTreeMap<String, Vec<Term>> = BTreeMap::new();
    for t in rdf::parse_turtle(text)? {
        if subject_iri(&t.subject) == Some(subject) {
            found
                .entry(t.predicate.as_str().to_string())
                .or_default()
                .push(t.object);
        }
    }
    if found.is_empty() {
        return Err(format!("nothing about <{subject}>"));
    }
    Ok(found)
}

fn one_literal(found: &BTreeMap<String, Vec<Term>>, p: &str) -> Option<String> {
    match found.get(p).map(Vec::as_slice) {
        Some([Term::Literal(l)]) => Some(l.value().to_string()),
        _ => None,
    }
}

fn one_iri(found: &BTreeMap<String, Vec<Term>>, p: &str) -> Option<String> {
    match found.get(p).map(Vec::as_slice) {
        Some([object]) => object_iri(object).map(str::to_string),
        _ => None,
    }
}

fn types(found: &BTreeMap<String, Vec<Term>>) -> Vec<String> {
    found
        .get(rdf::RDF_TYPE)
        .into_iter()
        .flatten()
        .filter_map(object_iri)
        .map(str::to_string)
        .collect()
}

/// Read a request from a tuple of `layer` (as `take` or `rd` answers it).
///
/// # Errors
///
/// A description of what is missing or wrong; a tuple the shapes admitted never has one.
pub fn parse_request(layer: &str, text: &str) -> std::result::Result<Asked, String> {
    let triples = rdf::parse_turtle(text)?;
    let prefix = format!("urn:layer:{layer}:request:");
    let subject = triples
        .iter()
        .filter_map(|t| subject_iri(&t.subject))
        .find(|s| s.starts_with(&prefix))
        .ok_or_else(|| format!("no request of layer `{layer}`"))?
        .to_string();
    let found = about(text, &subject)?;
    if !types(&found).contains(&term("Request")) {
        return Err(format!("<{subject}> is not a gw:Request"));
    }
    let correlation =
        one_literal(&found, &term("correlation")).ok_or("no single gw:correlation")?;
    if subject != request_subject(layer, &correlation) {
        return Err(format!(
            "<{subject}> carries gw:correlation `{correlation}`, which names another request"
        ));
    }
    let resource = one_iri(&found, &term("resource")).ok_or("no single gw:resource")?;
    let resource = Iri::parse(resource).map_err(|e| format!("gw:resource: {e:?}"))?;
    let verb = one_literal(&found, &ik("verb"))
        .and_then(|w| verb_of(&w))
        .ok_or("no single ik:verb of the five")?;
    let asker = one_iri(&found, rdf::PROV_ATTRIBUTED).ok_or("no prov:wasAttributedTo")?;
    let mut arguments = Vec::new();
    for value in found.get(&term("argument")).into_iter().flatten() {
        let Term::Literal(l) = value else {
            return Err("a gw:argument that is not a literal".into());
        };
        let (name, value) = l
            .value()
            .split_once('=')
            .ok_or("a gw:argument that is not name=value")?;
        arguments.push((name.to_string(), value.to_string()));
    }
    // The graph is a set: the order arguments were written in is not kept, so they are
    // passed on in name order (core's own arguments are a map).
    arguments.sort();
    Ok(Asked {
        subject,
        correlation,
        asker,
        ask: Ask {
            verb,
            resource,
            arguments,
            resolver: one_iri(&found, &rdf::ik_addressed_to()),
        },
    })
}

/// The answer to `asked`, as its resolver drops it, in Turtle: a `gw:Reply` carrying the
/// representation (its bytes in base64, its media type with parameters), or a `gw:Failure`
/// carrying the error's kind (one of [`ERROR_KINDS`]) and its parts. Either names the request
/// (`ik:offer`), is addressed to the asker, and is named [`reply_subject`].
///
/// ```
/// use ikigai_core::{Error, Iri, ReprType, Representation, Verb};
/// use ikigai_layers::gateway::{reply_turtle, Ask, Asked};
///
/// let asked = Asked {
///     subject: "urn:layer:gateway-a:request:c1".into(),
///     correlation: "c1".into(),
///     asker: "urn:collab:participant:a".into(),
///     ask: Ask::new(Verb::Source, Iri::parse("urn:data:a").unwrap()),
/// };
/// let has = |turtle: &str, p: &str, o: &str| {
///     ikigai_layers::rdf::parse_turtle(turtle).unwrap().iter().any(|t| {
///         t.subject.to_string() == "<urn:layer:gateway-a:reply:c1>"
///             && t.predicate.as_str() == p
///             && t.object.to_string() == o
///     })
/// };
/// let ok = reply_turtle("gateway-a", &asked, &Ok(Representation::new(ReprType::new("text/plain"), b"hi".to_vec())));
/// assert!(has(&ok, "http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:layers:gateway:Reply>"));
/// assert!(has(&ok, "urn:layers:gateway:bytes", "\"aGk=\"^^<http://www.w3.org/2001/XMLSchema#base64Binary>"));
/// assert!(has(&ok, "https://ikigai-rs.dev/ns#mediaType", "\"text/plain\""));
/// assert!(has(&ok, "https://ikigai-rs.dev/ns#offer", "<urn:layer:gateway-a:request:c1>"));
/// assert!(has(&ok, "https://ikigai-rs.dev/ns#addressedTo", "<urn:collab:participant:a>"));
///
/// let failed = reply_turtle("gateway-a", &asked, &Err(Error::NotFound("no a".into())));
/// assert!(has(&failed, "http://www.w3.org/1999/02/22-rdf-syntax-ns#type", "<urn:layers:gateway:Failure>"));
/// assert!(has(&failed, "urn:layers:gateway:errorKind", "\"not-found\""));
/// assert!(has(&failed, "urn:layers:gateway:message", "\"no a\""));
/// ```
pub fn reply_turtle(layer: &str, asked: &Asked, answer: &Result<Representation>) -> String {
    let subject = node(&reply_subject(layer, &asked.correlation));
    let mut triples = vec![
        Triple::new(
            subject.clone(),
            node(&rdf::ik_offer()),
            node(&asked.subject),
        ),
        Triple::new(
            subject.clone(),
            node(&rdf::ik_addressed_to()),
            node(&asked.asker),
        ),
        Triple::new(
            subject.clone(),
            node(&term("correlation")),
            Literal::new_simple_literal(&asked.correlation),
        ),
    ];
    let literal =
        |p: &str, v: String| Triple::new(subject.clone(), node(p), Literal::new_simple_literal(v));
    match answer {
        Ok(repr) => {
            triples.push(Triple::new(
                subject.clone(),
                node(rdf::RDF_TYPE),
                node(&term("Reply")),
            ));
            triples.push(literal(&ik("mediaType"), repr.repr_type.canonical()));
            triples.push(Triple::new(
                subject.clone(),
                node(&term("bytes")),
                Literal::new_typed_literal(
                    base64::engine::general_purpose::STANDARD.encode(&repr.bytes),
                    node(XSD_BASE64),
                ),
            ));
        }
        Err(error) => {
            triples.push(Triple::new(
                subject.clone(),
                node(rdf::RDF_TYPE),
                node(&term("Failure")),
            ));
            triples.push(literal(&term("errorKind"), error.kind().to_string()));
            let (message, argument, target, depth) = parts(error);
            if let Some(message) = message {
                triples.push(literal(&term("message"), cut(&message, MAX_MESSAGE)));
            }
            if let Some(argument) = argument {
                triples.push(literal(
                    &term("argumentName"),
                    cut(&argument, MAX_ARGUMENT_NAME),
                ));
            }
            if let Some(target) = target {
                triples.push(Triple::new(
                    subject.clone(),
                    node(&term("target")),
                    node(target.as_str()),
                ));
            }
            if let Some(depth) = depth {
                triples.push(Triple::new(
                    subject.clone(),
                    node(&term("depth")),
                    Literal::new_typed_literal(depth.to_string(), node(rdf::XSD_INTEGER)),
                ));
            }
        }
    }
    rdf::to_turtle_with(&triples, &[("gw", NS)])
}

type Parts<'a> = (Option<String>, Option<String>, Option<&'a Iri>, Option<u32>);

/// An error's payload, split into the failure's fields.
fn parts(error: &Error) -> Parts<'_> {
    match error {
        Error::Unresolved(iri) => (None, None, Some(iri), None),
        Error::MissingArgument(name) => (None, Some(name.clone()), None, None),
        Error::InvalidArgument { name, detail } => {
            (Some(detail.clone()), Some(name.clone()), None, None)
        }
        Error::Endpoint(m)
        | Error::Denied(m)
        | Error::NotFound(m)
        | Error::Conflict(m)
        | Error::Timeout(m)
        | Error::Unavailable(m) => (Some(m.clone()), None, None, None),
        Error::DepthExceeded { depth, target } => (None, None, Some(target), Some(*depth)),
        // `Error` is non-exhaustive: a variant core adds later crosses with its kind (which
        // the shape refuses until the closed set names it) and its display as the message.
        other => (Some(other.to_string()), None, None, None),
    }
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    text.chars().take(max).collect()
}

/// Read the answer to request `correlation` of `layer` from a tuple (as `take` answers it) or
/// a receipt holding it: the representation, or the typed error the resolver's kernel
/// answered, rebuilt as the same `Error` variant.
///
/// # Errors
///
/// The outer error describes a tuple that is not an answer to that request.
pub fn parse_reply(
    layer: &str,
    correlation: &str,
    text: &str,
) -> std::result::Result<Result<Representation>, String> {
    let subject = reply_subject(layer, correlation);
    let found = about(text, &subject)?;
    if one_literal(&found, &term("correlation")).as_deref() != Some(correlation) {
        return Err(format!(
            "<{subject}> does not carry gw:correlation `{correlation}`"
        ));
    }
    let kinds = types(&found);
    if kinds.contains(&term("Reply")) {
        let media = one_literal(&found, &ik("mediaType")).ok_or("no single ik:mediaType")?;
        let bytes = one_literal(&found, &term("bytes")).ok_or("no single gw:bytes")?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(bytes.as_bytes())
            .map_err(|e| format!("gw:bytes is not base64: {e}"))?;
        return Ok(Ok(Representation::new(repr_type(&media), bytes)));
    }
    if !kinds.contains(&term("Failure")) {
        return Err(format!(
            "<{subject}> is neither a gw:Reply nor a gw:Failure"
        ));
    }
    let kind = one_literal(&found, &term("errorKind")).ok_or("no single gw:errorKind")?;
    let message = one_literal(&found, &term("message")).unwrap_or_default();
    let argument = one_literal(&found, &term("argumentName"));
    let target = one_iri(&found, &term("target"))
        .map(|t| Iri::parse(t).map_err(|e| format!("gw:target: {e:?}")))
        .transpose()?;
    let need_target = || {
        target
            .clone()
            .ok_or_else(|| format!("a `{kind}` failure with no gw:target"))
    };
    Ok(Err(match kind.as_str() {
        "unresolved" => Error::Unresolved(need_target()?),
        "missing-argument" => Error::MissingArgument(argument.unwrap_or(message)),
        "invalid-argument" => Error::InvalidArgument {
            name: argument.unwrap_or_default(),
            detail: message,
        },
        "endpoint" => Error::Endpoint(message),
        "denied" => Error::Denied(message),
        "not-found" => Error::NotFound(message),
        "conflict" => Error::Conflict(message),
        "timeout" => Error::Timeout(message),
        "unavailable" => Error::Unavailable(message),
        "depth-exceeded" => Error::DepthExceeded {
            depth: one_literal(&found, &term("depth"))
                .and_then(|d| d.parse().ok())
                .ok_or("a depth-exceeded failure with no gw:depth")?,
            target: need_target()?,
        },
        other => return Err(format!("gw:errorKind `{other}` is not one of the ten")),
    }))
}

/// A media type with its parameters (`ReprType::canonical`'s spelling), parsed back.
fn repr_type(canonical: &str) -> ReprType {
    let mut pieces = canonical.split(';');
    let mut repr = ReprType::new(pieces.next().unwrap_or_default().trim());
    for param in pieces {
        if let Some((k, v)) = param.split_once('=') {
            repr = repr.with_param(k.trim(), v.trim());
        }
    }
    repr
}

/// An invalidation, as its owner says it on an event channel, in Turtle: `resource` changed,
/// because `reason`. About `<urn:layer:new>`; the host names it and stamps who, when, where.
pub fn invalidation_turtle(resource: &Iri, reason: &str) -> String {
    let subject = node(crate::name::NEW_SUBJECT);
    let triples = [
        Triple::new(
            subject.clone(),
            node(rdf::RDF_TYPE),
            node(&term("Invalidation")),
        ),
        Triple::new(
            subject.clone(),
            node(&term("resource")),
            node(resource.as_str()),
        ),
        Triple::new(
            subject,
            node(&term("reason")),
            Literal::new_simple_literal(reason),
        ),
    ];
    rdf::to_turtle_with(&triples, &[("gw", NS)])
}

/// How a participant reaches a kernel: one request in, the answer out, under the authority
/// the door holds. In-process that is a kernel and a capability ([`KernelDoor`]); over the
/// wire it is a mounted peer, whose door mints the principal.
#[async_trait]
pub trait Door: Send + Sync {
    /// Issue `request`.
    async fn call(&self, request: Request) -> Result<Representation>;
}

/// A [`Door`] onto a kernel in this process, under one capability.
pub struct KernelDoor<'k> {
    kernel: &'k Kernel,
    capability: Capability,
}

impl<'k> KernelDoor<'k> {
    /// `kernel`, under `capability`.
    pub fn new(kernel: &'k Kernel, capability: Capability) -> KernelDoor<'k> {
        KernelDoor { kernel, capability }
    }
}

#[async_trait]
impl Door for KernelDoor<'_> {
    async fn call(&self, request: Request) -> Result<Representation> {
        self.kernel.issue(request, &self.capability).await
    }
}

fn utf8(repr: &Representation) -> Result<String> {
    String::from_utf8(repr.bytes.clone())
        .map_err(|_| Error::Endpoint("the layer answered a non-UTF-8 representation".into()))
}

/// The JSON feed of `layer` after `after`, waiting up to `wait`: the new cursor (its `last`).
async fn wait_feed<D: Door + ?Sized>(
    door: &D,
    layer: &str,
    after: u64,
    wait: Duration,
) -> Result<u64> {
    // The feed waits in whole seconds, at most 60; a shorter deadline races it below.
    let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
    let secs = secs.min(crate::time::MAX_WAIT_SECS).to_string();
    let after = after.to_string();
    let repr = door
        .call(layer_request(
            Verb::Source,
            &format!("urn:layer:{layer}:feed"),
            &[
                ("after", after.as_bytes()),
                ("wait", secs.as_bytes()),
                ("as", b"application/json"),
            ],
        ))
        .await?;
    let json: serde_json::Value = serde_json::from_slice(&repr.bytes)
        .map_err(|e| Error::Endpoint(format!("layer `{layer}`'s feed is not JSON: {e}")))?;
    json.get("last")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| Error::Endpoint(format!("layer `{layer}`'s feed names no `last`")))
}

/// [`wait_feed`], cut short at `deadline`: `None` when the deadline came first.
async fn wait_feed_until<D: Door + ?Sized>(
    door: &D,
    layer: &str,
    after: u64,
    deadline: Instant,
) -> Result<Option<u64>> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(None);
    }
    let feed = Box::pin(wait_feed(door, layer, after, remaining));
    let timer = futures_timer::Delay::new(remaining);
    match select(feed, timer).await {
        Either::Left((last, _)) => last.map(Some),
        Either::Right(((), _)) => Ok(None),
    }
}

/// The ASKING participant on one ask layer: drops requests, waits for their answers.
pub struct Asker<D: Door> {
    door: D,
    layer: String,
    cursor: Mutex<u64>,
    minted: AtomicU64,
}

impl<D: Door> Asker<D> {
    /// The asker on `layer`, reaching the layer host through `door`: holding `out` (to ask),
    /// `take` (to take its answers, and to withdraw a request), `feed` (to wait) and `read`
    /// (to read back, from its receipt, an answer whose take reply was lost) on it.
    pub fn new(door: D, layer: &str) -> Asker<D> {
        Asker {
            door,
            layer: layer.to_string(),
            cursor: Mutex::new(0),
            minted: AtomicU64::new(0),
        }
    }

    /// A fresh correlation id: the time and a per-process count, unique for this asker.
    fn mint(&self) -> String {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or_default();
        let count = self.minted.fetch_add(1, Ordering::SeqCst);
        crate::id::mint(
            millis,
            (u64::from(std::process::id()) << 40) | (count & 0xff_ffff_ffff),
        )
    }

    /// Ask, under a fresh correlation id, and wait at most `deadline` for the answer: the
    /// representation the resolver's kernel answered, or its error, rebuilt as the same
    /// variant.
    ///
    /// # Errors
    ///
    /// The resolver's error, as above; the layer's refusal of the request (`InvalidArgument`
    /// when the shapes refuse it, `Denied` without the grant); and `Timeout` when no answer
    /// came within `deadline`. A request nobody took by then is WITHDRAWN (the asker takes it
    /// back, so no resolver runs it late); one a resolver already took is left to it, and its
    /// late answer stays addressed to this asker, pending.
    pub async fn ask(&self, ask: &Ask, deadline: Duration) -> Result<Representation> {
        // A reused name is a Conflict; another id is the cure (two askers of one principal
        // minting the same id in one millisecond is the only way here).
        let mut tries = 0;
        loop {
            match self.ask_as(&self.mint(), ask, deadline).await {
                Err(Error::Conflict(m)) if m.contains("already names") && tries < 3 => tries += 1,
                other => return other,
            }
        }
    }

    /// [`ask`](Self::ask) under the caller's own `correlation` id (a retry of a request whose
    /// reply was lost names the same one, and the host answers the replay as the first drop).
    ///
    /// # Errors
    ///
    /// As [`ask`](Self::ask); `InvalidArgument` for a correlation id the shapes refuse.
    pub async fn ask_as(
        &self,
        correlation: &str,
        ask: &Ask,
        deadline: Duration,
    ) -> Result<Representation> {
        let started = Instant::now();
        let deadline_at = started + deadline;
        let request = request_turtle(&self.layer, correlation, ask)?;
        let subject = request_subject(&self.layer, correlation);
        let reply = reply_subject(&self.layer, correlation);
        self.door
            .call(layer_request(
                Verb::Sink,
                &tuples(&self.layer),
                &[("content", request.as_bytes())],
            ))
            .await?;
        loop {
            if let Some(answer) = self.take_reply(correlation, &reply).await? {
                return answer;
            }
            let after = *self.cursor.lock().expect("cursor lock");
            match wait_feed_until(&self.door, &self.layer, after, deadline_at).await {
                Ok(Some(last)) => *self.cursor.lock().expect("cursor lock") = last,
                Ok(None) => break,
                // A cursor past the feed's end: this host's feed is not the one it came
                // from (restored, or another host). Start over from its beginning.
                Err(Error::Conflict(_)) => *self.cursor.lock().expect("cursor lock") = 0,
                Err(e) => return Err(e),
            }
        }
        // The deadline. Withdraw the request if nobody took it; if a resolver did, its
        // answer may have landed a moment ago.
        let withdrawn = self
            .door
            .call(layer_request(
                Verb::Delete,
                &tuples(&self.layer),
                &[("tuple", subject.as_bytes())],
            ))
            .await;
        let waited = format!("{:.3}s", started.elapsed().as_secs_f64());
        match withdrawn {
            Ok(_) => Err(Error::Timeout(format!(
                "no resolver took <{subject}> within {waited}; it is withdrawn"
            ))),
            Err(Error::Conflict(taken)) => {
                if let Some(answer) = self.take_reply(correlation, &reply).await? {
                    return answer;
                }
                Err(Error::Timeout(format!(
                    "no answer to <{subject}> within {waited} ({taken}); a late answer stays \
                     addressed to this asker"
                )))
            }
            // Addressed to one resolver, so only it can take it: it stays pending for it.
            Err(Error::Denied(why)) => Err(Error::Timeout(format!(
                "no answer to <{subject}> within {waited}, and it cannot be withdrawn ({why}): \
                 it stays pending for the resolver it is addressed to"
            ))),
            Err(e) => Err(e),
        }
    }

    /// Take the answer to `correlation` if it is there: `None` while it is not. An answer this
    /// asker already took (a take whose reply was lost) is read back from its receipt.
    async fn take_reply(
        &self,
        correlation: &str,
        reply: &str,
    ) -> Result<Option<Result<Representation>>> {
        let taken = self
            .door
            .call(layer_request(
                Verb::Delete,
                &tuples(&self.layer),
                &[("tuple", reply.as_bytes())],
            ))
            .await;
        let text = match taken {
            Ok(repr) => utf8(&repr)?,
            Err(Error::NotFound(_)) => return Ok(None),
            Err(Error::Conflict(_)) => utf8(
                &self
                    .door
                    .call(layer_request(
                        Verb::Source,
                        &tuples(&self.layer),
                        &[("state", b"taken"), ("tuple", reply.as_bytes())],
                    ))
                    .await?,
            )?,
            Err(e) => return Err(e),
        };
        parse_reply(&self.layer, correlation, &text)
            .map(Some)
            .map_err(|e| Error::Endpoint(format!("the answer <{reply}>: {e}")))
    }
}

/// The RESOLVING participant's own side: resolves a request in its own kernel, under its own
/// authority, within the verbs and prefixes its operator allows (see the module's notes).
pub struct Resolver<D: Door> {
    door: D,
    verbs: Vec<Verb>,
    prefixes: Vec<String>,
}

impl<D: Door> Resolver<D> {
    /// Resolve through `door` (the resolver's OWN kernel, under the capability its operator
    /// gave it), answering Source, Exists and Meta, for any name.
    pub fn new(door: D) -> Resolver<D> {
        Resolver {
            door,
            verbs: vec![Verb::Source, Verb::Exists, Verb::Meta],
            prefixes: Vec::new(),
        }
    }

    /// Answer exactly `verbs` (a mutating verb only when listed here).
    pub fn verbs(mut self, verbs: impl IntoIterator<Item = Verb>) -> Resolver<D> {
        self.verbs = verbs.into_iter().collect();
        self
    }

    /// Answer only names beginning with one of `prefixes`.
    pub fn prefixes<S: Into<String>>(
        mut self,
        prefixes: impl IntoIterator<Item = S>,
    ) -> Resolver<D> {
        self.prefixes = prefixes.into_iter().map(Into::into).collect();
        self
    }

    /// Resolve `asked` in this resolver's kernel.
    ///
    /// # Errors
    ///
    /// `Denied` for a verb or a name outside what the resolver allows; otherwise whatever its
    /// kernel answers.
    pub async fn resolve(&self, asked: &Asked) -> Result<Representation> {
        let ask = &asked.ask;
        if !self.verbs.contains(&ask.verb) {
            return Err(Error::Denied(format!(
                "this gateway's resolver does not answer {}: it answers {}",
                verb_word(ask.verb),
                self.verbs
                    .iter()
                    .map(|v| verb_word(*v))
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
        let name = ask.resource.as_str();
        if !self.prefixes.is_empty() && !self.prefixes.iter().any(|p| name.starts_with(p)) {
            return Err(Error::Denied(format!(
                "this gateway's resolver does not answer <{name}>: it answers names under {}",
                self.prefixes.join(", ")
            )));
        }
        let mut request = Request::new(ask.verb, ask.resource.clone());
        for (arg, value) in &ask.arguments {
            request = request.with_arg(arg.as_str(), ArgRef::Inline(value.as_bytes().to_vec()));
        }
        self.door.call(request).await
    }
}

/// What [`serve`] did with one request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Served {
    /// Taken, resolved and answered: the answer's subject.
    Answered(String),
    /// Another participant took it first (the layer's Conflict, naming who).
    Lost(String),
    /// Addressed to another resolver (the layer's Denied).
    NotOurs(String),
}

/// Take the request named `subject` from `layer` (through `layer_door`, holding `take` and
/// `out`), resolve it with `resolver`, and drop its answer.
///
/// An answer the layer refuses as content (a representation over the layer's `entry_max`) is
/// answered instead with a `gw:Failure` saying so, so the asker is never left waiting on a
/// reply that cannot be stored.
///
/// # Errors
///
/// The layer's, for anything but a lost race or another resolver's request; a request this
/// resolver took that is not one ([`parse_request`]) as `Endpoint`.
pub async fn serve<L: Door + ?Sized, R: Door>(
    layer_door: &L,
    layer: &str,
    resolver: &Resolver<R>,
    subject: &str,
) -> Result<Served> {
    let taken = layer_door
        .call(layer_request(
            Verb::Delete,
            &tuples(layer),
            &[("tuple", subject.as_bytes())],
        ))
        .await;
    let text = match taken {
        Ok(repr) => utf8(&repr)?,
        Err(Error::Conflict(m)) => return Ok(Served::Lost(m)),
        Err(Error::Denied(m)) => return Ok(Served::NotOurs(m)),
        Err(e) => return Err(e),
    };
    let asked = parse_request(layer, &text)
        .map_err(|e| Error::Endpoint(format!("took <{subject}>, which is not a request: {e}")))?;
    let answer = resolver.resolve(&asked).await;
    let drop_answer = |turtle: String| async move {
        layer_door
            .call(layer_request(
                Verb::Sink,
                &tuples(layer),
                &[("content", turtle.as_bytes())],
            ))
            .await
    };
    match drop_answer(reply_turtle(layer, &asked, &answer)).await {
        Ok(repr) => Ok(Served::Answered(utf8(&repr)?)),
        Err(Error::InvalidArgument { detail, .. }) if answer.is_ok() => {
            let failure = Err(Error::Endpoint(format!(
                "the answer could not cross layer `{layer}`: {detail}"
            )));
            let repr = drop_answer(reply_turtle(layer, &asked, &failure)).await?;
            Ok(Served::Answered(utf8(&repr)?))
        }
        Err(e) => Err(e),
    }
}

/// Serve every request pending in `layer`, in the order they were asked (`layer_door`
/// holding `read` as well as `take` and `out`).
///
/// # Errors
///
/// The first error [`serve`] answers.
pub async fn serve_pending<L: Door + ?Sized, R: Door>(
    layer_door: &L,
    layer: &str,
    resolver: &Resolver<R>,
) -> Result<Vec<Served>> {
    let prefix = format!("urn:layer:{layer}:request:");
    let mut served = Vec::new();
    let mut after = 0u64;
    loop {
        let after_text = after.to_string();
        let listed = utf8(
            &layer_door
                .call(layer_request(
                    Verb::Source,
                    &tuples(layer),
                    &[("after", after_text.as_bytes())],
                ))
                .await?,
        )?;
        // One line per pending tuple: `seq created subject dropper`.
        let lines: Vec<(u64, String)> = listed
            .lines()
            .filter_map(|line| {
                let mut words = line.split(' ');
                let seq = words.next()?.parse().ok()?;
                let subject = words.nth(1)?.to_string();
                Some((seq, subject))
            })
            .collect();
        let Some((last, _)) = lines.last() else {
            return Ok(served);
        };
        after = *last;
        for (_, subject) in lines.into_iter().filter(|(_, s)| s.starts_with(&prefix)) {
            served.push(serve(layer_door, layer, resolver, &subject).await?);
        }
    }
}

/// One step of a resolver that follows `layer`: wait up to `wait` for an event after `after`,
/// then serve every pending request (`layer_door` holding all four of a tuplespace's verbs:
/// `feed`, `read`, `take`, `out`). Answers the cursor for the next step. A resolver is this in
/// a loop, from 0.
///
/// # Errors
///
/// The feed's or [`serve_pending`]'s.
pub async fn follow<L: Door + ?Sized, R: Door>(
    layer_door: &L,
    layer: &str,
    resolver: &Resolver<R>,
    after: u64,
    wait: Duration,
) -> Result<u64> {
    let last = match wait_feed(layer_door, layer, after, wait).await {
        Ok(last) => last,
        // A cursor past the feed's end: start over (serving is idempotent: what was served
        // is no longer pending).
        Err(Error::Conflict(_)) => wait_feed(layer_door, layer, 0, Duration::ZERO).await?,
        Err(e) => return Err(e),
    };
    serve_pending(layer_door, layer, resolver).await?;
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_kind_round_trips() {
        let asked = Asked {
            subject: request_subject("gw", "c"),
            correlation: "c".into(),
            asker: "urn:collab:participant:a".into(),
            ask: Ask::new(Verb::Source, Iri::parse("urn:x").unwrap()),
        };
        let x = || Iri::parse("urn:x:y").unwrap();
        let errors = [
            Error::Unresolved(x()),
            Error::MissingArgument("in".into()),
            Error::InvalidArgument {
                name: "in".into(),
                detail: "bad".into(),
            },
            Error::Endpoint("boom".into()),
            Error::Denied("no".into()),
            Error::NotFound("gone".into()),
            Error::Conflict("taken".into()),
            Error::Timeout("slow".into()),
            Error::Unavailable("down".into()),
            Error::DepthExceeded {
                depth: 65,
                target: x(),
            },
        ];
        assert_eq!(errors.len(), ERROR_KINDS.len());
        for (error, kind) in errors.into_iter().zip(ERROR_KINDS) {
            assert_eq!(error.kind(), kind);
            let turtle = reply_turtle("gw", &asked, &Err(error.clone()));
            let back = parse_reply("gw", "c", &turtle).unwrap().unwrap_err();
            assert_eq!(back.to_string(), error.to_string(), "{kind}");
            assert_eq!(back.kind(), kind);
        }
    }

    #[test]
    fn a_representation_round_trips_with_its_parameters() {
        let asked = Asked {
            subject: request_subject("gw", "c"),
            correlation: "c".into(),
            asker: "urn:collab:participant:a".into(),
            ask: Ask::new(Verb::Source, Iri::parse("urn:x").unwrap()),
        };
        let bytes: Vec<u8> = (0..=255).collect();
        let repr = Representation::new(
            ReprType::new("application/octet-stream").with_param("charset", "binary"),
            bytes.clone(),
        );
        let turtle = reply_turtle("gw", &asked, &Ok(repr.clone()));
        let back = parse_reply("gw", "c", &turtle).unwrap().unwrap();
        assert_eq!(back, repr);
        assert_eq!(back.bytes, bytes);
    }

    #[test]
    fn a_request_round_trips() {
        let ask = Ask::new(Verb::Exists, Iri::parse("urn:data:a").unwrap())
            .arg("b", "x=y")
            .arg("a", "")
            .to("urn:collab:participant:r");
        let turtle = request_turtle("gw", "c.1", &ask).unwrap();
        let stamped = format!(
            "{turtle}\n<{}> <{}> <urn:collab:participant:a> .\n",
            request_subject("gw", "c.1"),
            rdf::PROV_ATTRIBUTED
        );
        let asked = parse_request("gw", &stamped).unwrap();
        assert_eq!(asked.asker, "urn:collab:participant:a");
        assert_eq!(asked.correlation, "c.1");
        // Arguments come back in name order, a value's own `=` kept.
        let mut expected = ask.clone();
        expected.arguments.sort();
        assert_eq!(asked.ask, expected);
    }

    #[test]
    fn what_the_shapes_would_refuse_is_refused_before_sending() {
        let ask = Ask::new(Verb::Source, Iri::parse("urn:data:a").unwrap());
        for bad in ["", "-a", "a b", "a/b", &"a".repeat(65)] {
            assert!(request_turtle("gw", bad, &ask).is_err(), "{bad:?}");
        }
        for name in ["", "1a", "a b", "a=b"] {
            assert!(
                request_turtle("gw", "c", &ask.clone().arg(name, "v")).is_err(),
                "{name:?}"
            );
        }
    }

    #[test]
    fn a_long_message_is_cut_to_the_shape() {
        let asked = Asked {
            subject: request_subject("gw", "c"),
            correlation: "c".into(),
            asker: "urn:collab:participant:a".into(),
            ask: Ask::new(Verb::Source, Iri::parse("urn:x").unwrap()),
        };
        let long = "é".repeat(MAX_MESSAGE + 10);
        let turtle = reply_turtle("gw", &asked, &Err(Error::Endpoint(long)));
        match parse_reply("gw", "c", &turtle).unwrap() {
            Err(Error::Endpoint(m)) => assert_eq!(m.chars().count(), MAX_MESSAGE),
            other => panic!("{other:?}"),
        }
    }
}
