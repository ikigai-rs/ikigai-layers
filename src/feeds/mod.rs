//! Feed layers (ledger #3284): the host's one shared copy of what a feed answers.
//!
//! A feed is a channel with `feed = true`, read by many and written only by its `publishers`.
//! A door's kernel keys its cache by the caller's whole capability, principal included, so a
//! feed read cached there would be one copy per reader, and `Capability::attenuate` cannot
//! produce a capability that two readers share. So this module holds a kernel of the HOST'S
//! OWN, bound into no space a door serves (the pattern [`crate::json::Egress`] set): a feed's
//! reads run there under the host's authority narrowed to `urn:cap:layer:read:{layer}` with no
//! principal, so one request has one cache entry whoever asked. The door's endpoint checks the
//! caller's own `urn:cap:layer:read:{layer}` first and answers from this copy without keeping
//! one of its own, so nothing is cached per reader.
//!
//! The reads hang from the layer's say thread, which the host cuts after every
//! committed `say` before warming the default `entries` page and the `graph`: the first reader
//! after a publish hits the cache, and read latency says nothing about other readers.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use ikigai_core::{
    Capability, Description, Endpoint, Error, Exact, Fallback, Invocation, Kernel, ReprType,
    Representation, Request, Result, Space, Verb,
};
use oxrdf::{Literal, Triple};

use crate::endpoints;
use crate::host::{Host, LayerState};
use crate::name::{layer_cap, layer_iri};
use crate::rdf::{self, node};

/// The SPARQL CONSTRUCT that lifts a feed's entries to ikigai-feed's form, unioned with its
/// scheme.
pub const GRAPH_QUERY: &str = include_str!("graph.rq");
/// The SPARQL SELECT behind `entries concept=` and the Atom face; see [`entries_query`].
pub const ENTRIES_QUERY: &str = include_str!("entries.rq");
/// The stylesheet that writes the SELECT's rows as an Atom 1.0 document.
pub const ATOM_XSLT: &str = include_str!("atom.xsl");
/// The JSON-LD context of `urn:layer:{layer}:graph`, whatever the layer's own egress context.
pub const GRAPH_CONTEXT: &str = include_str!("graph.context.jsonld");

/// The media type of the Atom face.
pub const ATOM: &str = "application/atom+xml";

const TURTLE: &str = "text/turtle";

/// The resource holding what the graph query reads: the feed's definition, its scheme and every
/// entry. Bound only in the private kernel.
fn source_iri(layer: &str) -> String {
    format!("{}:source", layer_iri(layer))
}

/// The [`ENTRIES_QUERY`] for these already-checked values: `concepts` are concepts of the
/// layer's scheme (so their IRIs are the operator's, and no IRI can hold a `>`), or empty for
/// no filter; `after` and `limit` are numbers.
///
/// ```
/// let q = ikigai_layers::feeds::entries_query(&["urn:example:c".to_string()], 3, 7);
/// assert!(q.contains("VALUES ?wanted { <urn:example:c> }"));
/// assert!(q.contains("FILTER(?seq > 3)"));
/// assert!(q.contains("LIMIT 7"));
/// assert!(ikigai_layers::feeds::entries_query(&[], 0, 1).contains("VALUES ?wanted { UNDEF }"));
/// ```
pub fn entries_query(concepts: &[String], after: u64, limit: u64) -> String {
    let cells = if concepts.is_empty() {
        "UNDEF".to_string()
    } else {
        concepts
            .iter()
            .map(|c| format!("<{c}>"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    ENTRIES_QUERY
        .replace("__CONCEPTS__", &cells)
        .replace("__AFTER__", &after.to_string())
        .replace("__LIMIT__", &limit.to_string())
}

/// What `ikigai_xslt` turned the SELECT's rows into, or the host's own error.
pub(crate) fn atom_from_rows(rows: &str) -> Result<String> {
    ikigai_xslt::transform_xml(rows, ATOM_XSLT, false)
        .map_err(|e| Error::Endpoint(format!("the feed's Atom stylesheet failed: {e}")))
}

/// Every feed layer's one shared copy: the kernel, and how often each layer's reads were
/// computed rather than served from it.
pub(crate) struct FeedShare {
    kernel: Kernel,
    computed: BTreeMap<String, Arc<AtomicU64>>,
}

impl FeedShare {
    /// The share over `layers`' feeds. The kernel holds the host only weakly: the host owns this
    /// kernel, and a strong reference back would keep it, and its state directory's lock, alive
    /// forever.
    pub(crate) fn new(host: Weak<Host>, layers: &BTreeMap<String, Arc<LayerState>>) -> FeedShare {
        let mut computed = BTreeMap::new();
        let mut spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_sparql::space_with_budget(
            std::time::Duration::from_secs(10),
        ))];
        for (name, state) in layers.iter().filter(|(_, s)| s.def.is_feed()) {
            computed.insert(name.clone(), Arc::new(AtomicU64::new(0)));
            let space = endpoints::feed_share_space(host.clone(), Arc::clone(state)).bind(
                Exact::new(source_iri(name)),
                FeedSource {
                    state: Arc::clone(state),
                },
            );
            spaces.push(Arc::new(space));
        }
        FeedShare {
            kernel: Kernel::new(Arc::new(Fallback::new(spaces))),
            computed,
        }
    }

    /// `layer`'s read token under the host's own authority: no principal, so one request has
    /// one cache entry for every reader.
    fn authority(layer: &str) -> Capability {
        Capability::root().attenuate([layer_cap("read", layer)])
    }

    /// Answer a feed read from the shared copy, computing it only when it is not there.
    pub(crate) async fn read(&self, layer: &str, request: Request) -> Result<Representation> {
        self.kernel.issue(request, &Self::authority(layer)).await
    }

    /// One computation of one of `layer`'s reads (a read served from the copy counts nothing).
    pub(crate) fn count(&self, layer: &str) {
        if let Some(counter) = self.computed.get(layer) {
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// How many of `layer`'s reads were computed rather than served from the copy.
    pub(crate) fn computed(&self, layer: &str) -> u64 {
        self.computed
            .get(layer)
            .map_or(0, |c| c.load(Ordering::Relaxed))
    }

    /// A `say` committed on `layer`: every answer computed from the old entries is stale, and
    /// the default `entries` page and the `graph` are computed now, so the first reader finds
    /// them. A failed warm costs only that first reader a computation, and the entry stands.
    pub(crate) async fn published(&self, layer: &str) {
        self.kernel.cut(endpoints::say_thread_of(layer));
        for request in [
            endpoints::feed_entries_request(layer),
            endpoints::feed_graph_request(layer),
        ] {
            if let Err(e) = self.read(layer, request).await {
                eprintln!(
                    "ikigai-layers: feed `{layer}`: warming its shared copy after a publish \
                     failed ({e}); the first reader computes it instead"
                );
            }
        }
    }
}

/// `urn:layer:{layer}:source`, private to the [`FeedShare`] kernel: the feed's definition, its
/// scheme and every stored entry as Turtle, for the graph query to read.
struct FeedSource {
    state: Arc<LayerState>,
}

#[async_trait]
impl Endpoint for FeedSource {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        let def = &self.state.def;
        let layer = node(&layer_iri(&def.name));
        let about = [
            Triple::new(
                layer.clone(),
                node(rdf::RDF_TYPE),
                node(&rdf::ik_feed_class()),
            ),
            Triple::new(
                layer,
                node(rdf::RDFS_LABEL),
                Literal::new_simple_literal(def.label.as_deref().unwrap_or(&def.name)),
            ),
        ];
        let mut text = rdf::to_turtle(about.iter());
        if let Some(feed) = &def.feed {
            text.push_str(&feed.scheme.turtle);
        }
        {
            let channel = self.state.channel().read().expect("channel lock");
            for meta in channel.entries() {
                text.push_str(&channel.turtle(meta).map_err(Error::Endpoint)?);
            }
        }
        Ok(
            Representation::new(ReprType::new(TURTLE), text.into_bytes())
                .cacheable()
                .depends_on(endpoints::say_thread_of(&def.name)),
        )
    }

    fn name(&self) -> &str {
        "feed-source"
    }

    fn describe(&self) -> Description {
        Description::new(format!("layer-{}-feed-source", self.state.def.name))
            .verb(Verb::Source)
            .requires(layer_cap("read", &self.state.def.name))
            .output(TURTLE)
    }
}

#[cfg(test)]
mod tests {
    use super::atom_from_rows;

    // Fixed SELECT rows keep timestamps and entry identifiers stable across engine upgrades.
    const FEED: &str = r#"<binding name="layer"><uri>urn:layer:releases</uri></binding><binding name="feedLabel"><literal>Releases &amp; News</literal></binding>"#;

    fn rows(results: &str) -> String {
        format!(
            r#"<sparql xmlns="http://www.w3.org/2005/sparql-results#"><head/><results>{results}</results></sparql>"#
        )
    }

    #[test]
    fn empty_atom_is_byte_stable() {
        let xml = rows(&format!("<result>{FEED}</result>"));
        assert_eq!(
            atom_from_rows(&xml).unwrap(),
            concat!(
                "<feed xmlns='http://www.w3.org/2005/Atom'>",
                "<id>urn:layer:releases</id>",
                "<title>Releases &amp; News</title>",
                "<updated>1970-01-01T00:00:00Z</updated>",
                "</feed>",
            )
        );
    }

    #[test]
    fn populated_atom_is_byte_stable() {
        let xml = rows(&format!(
            r#"<result>{FEED}<binding name="entry"><uri>urn:entry:First</uri></binding><binding name="title"><literal>Release &amp; News</literal></binding><binding name="issued"><literal>2026-11-10T12:00:00Z</literal></binding><binding name="creator"><uri>urn:participant:Brian</uri></binding><binding name="content"><literal>Read &lt;this&gt;.</literal></binding></result><result>{FEED}<binding name="entry"><uri>urn:entry:Second</uri></binding><binding name="issued"><literal>2026-11-10T13:00:00Z</literal></binding><binding name="creator"><uri>urn:participant:Brian</uri></binding></result>"#
        ));
        assert_eq!(
            atom_from_rows(&xml).unwrap(),
            concat!(
                "<feed xmlns='http://www.w3.org/2005/Atom'>",
                "<id>urn:layer:releases</id>",
                "<title>Releases &amp; News</title>",
                "<updated>2026-11-10T13:00:00Z</updated>",
                "<entry>",
                "<id>urn:entry:First</id>",
                "<title>Release &amp; News</title>",
                "<updated>2026-11-10T12:00:00Z</updated>",
                "<published>2026-11-10T12:00:00Z</published>",
                "<author>",
                "<name>urn:participant:Brian</name>",
                "</author>",
                "<content type='text'>Read &lt;this&gt;.</content>",
                "</entry>",
                "<entry>",
                "<id>urn:entry:Second</id>",
                "<title>(untitled)</title>",
                "<updated>2026-11-10T13:00:00Z</updated>",
                "<published>2026-11-10T13:00:00Z</published>",
                "<author>",
                "<name>urn:participant:Brian</name>",
                "</author></entry>",
                "</feed>",
            )
        );
    }
}
