//! The JSON faces: every answer a participant reads has a form a participant with only an
//! HTTP client and a JSON parser can use (ledger #1371).
//!
//! - **A graph answer** (`entries`, `entry:{id}`, `graph:{slug}`, `latest`, `rd` with
//!   `tuple=` or `state=taken`, `take`, `receipt:{id}`) answers `as=application/ld+json`
//!   with the layer's egress context, PRUNED and compacted: the stored graph is expanded,
//!   everything the context does not define is removed, and what is left is compacted with
//!   that context. ikigai-jsonld's `urn:jsonld:prune` does it, in a kernel this module owns
//!   and binds nowhere else (see [`Egress`]): no participant can call the pruner itself.
//! - **A list** (`rd`'s pending tuples and receipts) and **a write's answer** (`say`, `out`)
//!   answer `as=application/json` with plain JSON.
//!
//! # The shape of a graph answer
//!
//! Always one object with exactly two members: `@context` (the context the body was
//! compacted with: the layer's own, with the host's [`STAMP_CONTEXT`] merged in) and
//! `@graph`, an ARRAY of node objects, so one node and many look the same and no node is
//! nested inside another. In each node:
//!
//! - `@id` is the node's IRI, whole; `@type` is a term of the context, a string for one type
//!   and an array for several;
//! - every other key is a term of the context (never a full IRI), and a property the
//!   context does not define is absent;
//! - a property with one value is that value, with several an array (a term whose
//!   definition says `"@container": "@set"` is always an array);
//! - an IRI value is a string when its term says `"@type": "@id"`, otherwise
//!   `{"@id": …}`;
//! - a typed literal whose term declares its datatype is its LEXICAL STRING (`"seq": "3"`,
//!   `"created": "2026-10-10T00:00:01.000Z"`, base64 bytes as their text); a datatype no term
//!   of the context declares is removed with its value.
//!
//! Every node the host stamped carries the host's terms, whatever the layer's context
//! names: `attributedTo` (who said or dropped it), `created` and `seq`; a receipt is an
//! `Activity` with `used` (the tuple), `associatedWith` (the taker), `endedAt` and `seq`.
//! A layer context that defines one of these terms differently, or maps one of these IRIs
//! under another name, is refused when the host opens ([`Egress::check`]).
//!
//! ```
//! use ikigai_layers::json::shape;
//! use serde_json::json;
//!
//! // One node, as a compaction answers it, becomes a one-element @graph...
//! let one = shape(json!({"@context": {"seq": "https://ikigai-rs.dev/ns#seq"},
//!                        "@id": "urn:layer:talk:entry:1", "seq": "1"}));
//! assert_eq!(one, json!({"@context": {"seq": "https://ikigai-rs.dev/ns#seq"},
//!                        "@graph": [{"@id": "urn:layer:talk:entry:1", "seq": "1"}]}));
//! // ...and nothing is an empty one.
//! let none = shape(json!({"@context": {}}));
//! assert_eq!(none, json!({"@context": {}, "@graph": []}));
//! ```

use std::sync::Arc;

use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Request, Result, Verb};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use serde_json::{json, Map, Value};

use crate::rdf;

/// The terms the host stamps with, merged into every layer's context for its JSON-LD
/// faces, so a stamp crosses whatever the layer's own context names.
///
/// ```
/// let stamps: serde_json::Value =
///     serde_json::from_str(ikigai_layers::json::STAMP_CONTEXT).unwrap();
/// let terms = stamps["@context"].as_object().unwrap();
/// let names: Vec<&str> = terms.keys().map(String::as_str).collect();
/// assert_eq!(
///     names,
///     ["Activity", "associatedWith", "attributedTo", "created", "endedAt", "seq", "used"]
/// );
/// assert_eq!(terms["seq"]["@id"], "https://ikigai-rs.dev/ns#seq");
/// assert_eq!(terms["seq"]["@type"], "http://www.w3.org/2001/XMLSchema#integer");
/// ```
pub const STAMP_CONTEXT: &str = r#"{ "@context": {
  "Activity": "http://www.w3.org/ns/prov#Activity",
  "associatedWith": { "@id": "http://www.w3.org/ns/prov#wasAssociatedWith", "@type": "@id" },
  "attributedTo": { "@id": "http://www.w3.org/ns/prov#wasAttributedTo", "@type": "@id" },
  "created": { "@id": "http://purl.org/dc/terms/created", "@type": "http://www.w3.org/2001/XMLSchema#dateTime" },
  "endedAt": { "@id": "http://www.w3.org/ns/prov#endedAtTime", "@type": "http://www.w3.org/2001/XMLSchema#dateTime" },
  "seq": { "@id": "https://ikigai-rs.dev/ns#seq", "@type": "http://www.w3.org/2001/XMLSchema#integer" },
  "used": { "@id": "http://www.w3.org/ns/prov#used", "@type": "@id" }
} }"#;

const PRUNE: &str = "urn:jsonld:prune";

/// The context a layer's JSON-LD faces compact with: the layer's own, with the host's
/// [`STAMP_CONTEXT`] terms merged in. A layer context that is one object becomes one object
/// (the layer's definition of a term wins, which [`Egress::check`] then holds to the host's
/// meaning); an array keeps its order after the host's terms.
///
/// # Errors
/// A context that is not JSON, or whose `@context` is not an object, an array or null.
pub fn egress_context(layer_context: &str) -> std::result::Result<Value, String> {
    let layer: Value = serde_json::from_str(layer_context).map_err(|e| format!("not JSON: {e}"))?;
    let mut stamps = stamp_terms();
    Ok(
        match layer.get("@context").cloned().unwrap_or(Value::Null) {
            Value::Object(terms) => {
                stamps.extend(terms);
                Value::Object(stamps)
            }
            Value::Array(items) => {
                let mut merged = vec![Value::Object(stamps)];
                merged.extend(items);
                Value::Array(merged)
            }
            Value::Null => Value::Object(stamps),
            _ => return Err("a context is an object, an array of objects, or null".into()),
        },
    )
}

fn stamp_terms() -> Map<String, Value> {
    match serde_json::from_str::<Value>(STAMP_CONTEXT) {
        Ok(Value::Object(mut doc)) => match doc.remove("@context") {
            Some(Value::Object(terms)) => terms,
            _ => unreachable!("STAMP_CONTEXT has an object @context"),
        },
        _ => unreachable!("STAMP_CONTEXT is a JSON object"),
    }
}

/// A graph as expanded JSON-LD: one node object per subject, in the order subjects first
/// appear, each property an array of value objects. This is the input the prune expects, so
/// nothing is lost before the context decides what crosses: a plain literal is
/// `{"@value": …}`, a typed one carries its datatype IRI, a language-tagged one its tag.
pub fn expanded(triples: &[Triple]) -> Value {
    let mut order: Vec<String> = Vec::new();
    let mut nodes: std::collections::HashMap<String, Map<String, Value>> =
        std::collections::HashMap::new();
    for triple in triples {
        let subject = match &triple.subject {
            NamedOrBlankNode::NamedNode(n) => n.as_str().to_string(),
            NamedOrBlankNode::BlankNode(b) => format!("_:{}", b.as_str()),
        };
        let node = nodes.entry(subject.clone()).or_insert_with(|| {
            order.push(subject.clone());
            let mut node = Map::new();
            node.insert("@id".into(), Value::String(subject));
            node
        });
        let predicate = triple.predicate.as_str();
        let value = if let Term::NamedNode(n) = &triple.object {
            if predicate == rdf::RDF_TYPE {
                push(node, "@type", Value::String(n.as_str().to_string()));
                continue;
            }
            json!({ "@id": n.as_str() })
        } else if let Term::BlankNode(b) = &triple.object {
            json!({ "@id": format!("_:{}", b.as_str()) })
        } else if let Term::Literal(l) = &triple.object {
            match (l.language(), l.datatype().as_str()) {
                (Some(language), _) => json!({ "@value": l.value(), "@language": language }),
                (None, rdf::XSD_STRING) => json!({ "@value": l.value() }),
                (None, datatype) => json!({ "@value": l.value(), "@type": datatype }),
            }
        } else {
            // A quoted triple (RDF 1.2) has no JSON-LD 1.1 form, and no layer stores one.
            continue;
        };
        push(node, predicate, value);
    }
    Value::Array(
        order
            .into_iter()
            .filter_map(|s| nodes.remove(&s))
            .map(Value::Object)
            .collect(),
    )
}

fn push(node: &mut Map<String, Value>, key: &str, value: Value) {
    match node
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(values) => values.push(value),
        _ => unreachable!("every member but @id is an array"),
    }
}

/// The fixed shape of a graph answer: `{"@context": …, "@graph": [nodes]}`, whatever the
/// compaction answered (a compaction answers one node as a top-level object and none as an
/// object holding only `@context`).
pub fn shape(compacted: Value) -> Value {
    let Value::Object(mut doc) = compacted else {
        // A compaction always answers an object; anything else is passed through as data.
        return json!({ "@context": {}, "@graph": [compacted] });
    };
    let context = doc.remove("@context").unwrap_or_else(|| json!({}));
    let nodes = match doc.remove("@graph") {
        Some(Value::Array(nodes)) if doc.is_empty() => nodes,
        Some(other) => {
            doc.insert("@graph".into(), other);
            vec![Value::Object(doc)]
        }
        None if doc.is_empty() => Vec::new(),
        None => vec![Value::Object(doc)],
    };
    json!({ "@context": context, "@graph": nodes })
}

/// The host's own JSON-LD processor: a kernel binding ikigai-jsonld's space and nothing
/// else, owned by the host and bound into no space a door serves. Its `urn:jsonld:prune`
/// declares no capability, so a door that bound it would offer it to every participant
/// (the shape of ledger #1266); here only the host's own faces reach it, under an empty
/// capability, with an inline context (it resolves nothing).
pub struct Egress {
    kernel: Kernel,
}

impl Default for Egress {
    fn default() -> Self {
        Egress::new()
    }
}

impl Egress {
    /// A processor of its own.
    pub fn new() -> Egress {
        Egress {
            kernel: Kernel::new(Arc::new(ikigai_jsonld::space())),
        }
    }

    /// `triples`, pruned to `context` (an [`egress_context`]) and compacted with it, in
    /// [`shape`]. An empty graph answers an empty `@graph` without running the prune.
    ///
    /// # Errors
    /// The prune's own refusal, as it answered it (a context it cannot read an allowlist
    /// from), or `Endpoint` when its answer is not JSON.
    pub async fn compact(&self, context: &Value, triples: &[Triple]) -> Result<Value> {
        if triples.is_empty() {
            return Ok(json!({ "@context": context, "@graph": [] }));
        }
        let request = Request::new(Verb::Source, Iri::parse(PRUNE).expect("a constant IRI"))
            .with_arg(
                "content",
                ArgRef::Inline(expanded(triples).to_string().into_bytes()),
            )
            .with_arg(
                "context",
                ArgRef::Inline(json!({ "@context": context }).to_string().into_bytes()),
            );
        let answer = self
            .kernel
            .issue(request, &Capability::scoped(Vec::<String>::new()))
            .await?;
        let compacted: Value = serde_json::from_slice(&answer.bytes)
            .map_err(|e| Error::Endpoint(format!("the JSON-LD prune answered no JSON: {e}")))?;
        Ok(shape(compacted))
    }

    /// Hold layer `layer`'s merged `context` to the host's meaning of its stamps: a stamped
    /// probe must come out under exactly the host's terms, with exactly its values. Refuses
    /// a context the prune cannot read (a scoped `@context`, a term JSON-LD rejects), one that
    /// gives a stamp term another IRI or datatype, one that maps a stamp's IRI under another
    /// (shorter, so preferred) name, and one whose prefixes would shorten the IRIs the host
    /// names (entries, receipts, participants).
    ///
    /// # Errors
    /// Why, naming the term.
    pub fn check(&self, layer: &str, context: &Value) -> std::result::Result<(), String> {
        let node = |p: &str| oxrdf::NamedNode::new(p).expect("a constant IRI");
        let probe = format!("urn:layer:{layer}:receipt:probe");
        let used = format!("urn:layer:{layer}:entry:probe");
        let who = "urn:collab:participant:probe";
        let machine = "urn:iki:host:probe:client:probe";
        let time = "2026-01-01T00:00:00.000Z";
        let s = node(&probe);
        let typed = |v: &str, dt: &str| oxrdf::Literal::new_typed_literal(v, node(dt));
        let triples = vec![
            Triple::new(s.clone(), node(rdf::RDF_TYPE), node(rdf::PROV_ACTIVITY)),
            Triple::new(s.clone(), node(rdf::PROV_ATTRIBUTED), node(who)),
            Triple::new(
                s.clone(),
                node(rdf::DCT_CREATED),
                typed(time, rdf::XSD_DATE_TIME),
            ),
            Triple::new(
                s.clone(),
                node(&rdf::ik_seq()),
                typed("1", rdf::XSD_INTEGER),
            ),
            Triple::new(s.clone(), node(rdf::PROV_USED), node(&used)),
            Triple::new(s.clone(), node(rdf::PROV_ASSOCIATED), node(machine)),
            Triple::new(s, node(rdf::PROV_ENDED), typed(time, rdf::XSD_DATE_TIME)),
        ];
        let doc = futures::executor::block_on(self.compact(context, &triples))
            .map_err(|e| format!("its context cannot be a JSON-LD egress context: {e}"))?;
        let empty = Map::new();
        let got = doc["@graph"]
            .as_array()
            .and_then(|nodes| nodes.first())
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let expected = [
            ("@id", probe.as_str()),
            ("@type", "Activity"),
            ("attributedTo", who),
            ("created", time),
            ("seq", "1"),
            ("used", used.as_str()),
            ("associatedWith", machine),
            ("endedAt", time),
        ];
        for (term, value) in expected {
            if got.get(term) != Some(&Value::String(value.into())) {
                let name = if term == "@type" { "Activity" } else { term };
                return Err(format!(
                    "its context changes the host's term `{name}` (a stamp every JSON-LD face \
                     carries as {}): it defines that term, or its IRI under another name, \
                     differently, or a prefix of it shortens the IRIs the host names; the \
                     probe came out as {}",
                    serde_json::to_string(&json!({ term: value })).unwrap_or_default(),
                    Value::Object(got.clone())
                ));
            }
        }
        if got.len() != expected.len() {
            return Err(format!(
                "its context renames one of the host's stamps (attributedTo, created, seq, \
                 Activity, used, associatedWith, endedAt); the probe came out as {}",
                Value::Object(got.clone())
            ));
        }
        Ok(())
    }
}
