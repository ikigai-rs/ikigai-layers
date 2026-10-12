//! The RDF this crate reads and writes: the terms it stamps, and Turtle in and out.

use oxrdf::{NamedNode, NamedOrBlankNode, Term, Triple};
use oxttl::{TurtleParser, TurtleSerializer};

/// The ikigai vocabulary namespace (`ik:`), published at <https://ikigai-rs.dev/ns>.
pub const IK: &str = "https://ikigai-rs.dev/ns#";
/// `rdf:type`.
pub const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
/// `rdfs:label`.
pub const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
/// `rdfs:seeAlso`.
pub const RDFS_SEE_ALSO: &str = "http://www.w3.org/2000/01/rdf-schema#seeAlso";
/// `prov:wasAttributedTo`: who said an entry, stamped by the host.
pub const PROV_ATTRIBUTED: &str = "http://www.w3.org/ns/prov#wasAttributedTo";
/// `dcterms:created`: when, by the kernel clock, stamped by the host.
pub const DCT_CREATED: &str = "http://purl.org/dc/terms/created";
/// `dcterms:subject`: what an entry is about, the default for a layer's `latest about=`.
pub const DCT_SUBJECT: &str = "http://purl.org/dc/terms/subject";
/// `dcterms:conformsTo`: a layer to its admission shapes.
pub const DCT_CONFORMS_TO: &str = "http://purl.org/dc/terms/conformsTo";
/// `dcterms:hasPart`: a collaboration to a layer it mounts.
pub const DCT_HAS_PART: &str = "http://purl.org/dc/terms/hasPart";
/// `dcterms:isPartOf`: a shared layer to a collaboration its definition is shared with.
pub const DCT_IS_PART_OF: &str = "http://purl.org/dc/terms/isPartOf";
/// `dcterms:contributor`: a collaboration to a participant it grants anything.
pub const DCT_CONTRIBUTOR: &str = "http://purl.org/dc/terms/contributor";
/// `prov:Activity`: a take, as its receipt records it.
pub const PROV_ACTIVITY: &str = "http://www.w3.org/ns/prov#Activity";
/// `prov:used`: a take to the tuple it removed.
pub const PROV_USED: &str = "http://www.w3.org/ns/prov#used";
/// `prov:wasAssociatedWith`: a take to its taker (the principal its door minted).
pub const PROV_ASSOCIATED: &str = "http://www.w3.org/ns/prov#wasAssociatedWith";
/// `prov:endedAtTime`: when a take happened, by the kernel clock.
pub const PROV_ENDED: &str = "http://www.w3.org/ns/prov#endedAtTime";
/// `foaf:primaryTopic`: `urn:layers:me` to the principal it describes.
pub const FOAF_PRIMARY_TOPIC: &str = "http://xmlns.com/foaf/0.1/primaryTopic";
/// `foaf:Person`: a participant who is a person.
pub const FOAF_PERSON: &str = "http://xmlns.com/foaf/0.1/Person";
/// `prov:SoftwareAgent`: a participant that is an agent acting for a person.
pub const PROV_SOFTWARE_AGENT: &str = "http://www.w3.org/ns/prov#SoftwareAgent";
/// `prov:actedOnBehalfOf`: an agent or a machine to the participant it acts for.
pub const PROV_ACTED_FOR: &str = "http://www.w3.org/ns/prov#actedOnBehalfOf";
/// `xsd:dateTime`.
pub const XSD_DATE_TIME: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
/// `xsd:integer`.
pub const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";
/// `xsd:string`.
pub const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// `sh:NodeShape`.
pub const SH_NODE_SHAPE: &str = "http://www.w3.org/ns/shacl#NodeShape";
/// `sh:closed`.
pub const SH_CLOSED: &str = "http://www.w3.org/ns/shacl#closed";
/// `sh:targetClass`.
pub const SH_TARGET_CLASS: &str = "http://www.w3.org/ns/shacl#targetClass";

/// `ik:seq`: an entry's place in its layer's feed, stamped by the host.
pub fn ik_seq() -> String {
    format!("{IK}seq")
}

/// `ik:supersedes`.
pub fn ik_supersedes() -> String {
    format!("{IK}supersedes")
}

/// `ik:addressedTo`: the participant a tuple is for. In an `addressed` tuplespace only a
/// participant it names may take it.
pub fn ik_addressed_to() -> String {
    format!("{IK}addressedTo")
}

/// `ik:offer`: the offer a report is about.
pub fn ik_offer() -> String {
    format!("{IK}offer")
}

/// `ik:Layer`. ⚠ Never `ik:layer` (lowercase): that is a dry run's chain cell with
/// `rdfs:domain ik:Consultation`, and writing it would type the subject a consultation.
pub fn ik_layer_class() -> String {
    format!("{IK}Layer")
}

/// `ik:layerModel`.
pub fn ik_layer_model() -> String {
    format!("{IK}layerModel")
}

/// `ik:Machine`: a participant that is a running host acting under its own identity.
pub fn ik_machine() -> String {
    format!("{IK}Machine")
}

/// `ik:Collaboration`.
pub fn ik_collaboration() -> String {
    format!("{IK}Collaboration")
}

/// A named node from a string this crate built itself (a constant or a minted IRI).
///
/// # Panics
/// On a string that is not an IRI: every caller passes a constant or an IRI it built from
/// validated names, so a panic here is a bug in this crate, never caller input.
pub fn node(iri: &str) -> NamedNode {
    NamedNode::new(iri).unwrap_or_else(|e| panic!("`{iri}` is not an IRI: {e}"))
}

/// Parse Turtle into triples. No base IRI: a relative IRI is an error, never resolved
/// against something the author did not write.
pub fn parse_turtle(text: &str) -> Result<Vec<Triple>, String> {
    TurtleParser::new()
        .for_slice(text.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

/// Serialize triples as Turtle with the prefixes this crate's graphs use.
pub fn to_turtle<'a>(triples: impl IntoIterator<Item = &'a Triple>) -> String {
    to_turtle_with(triples, &[])
}

/// [`to_turtle`], with more prefixes.
pub fn to_turtle_with<'a>(
    triples: impl IntoIterator<Item = &'a Triple>,
    extra: &[(&str, &str)],
) -> String {
    let mut serializer = TurtleSerializer::new();
    for (prefix, ns) in [
        ("ik", IK),
        ("prov", "http://www.w3.org/ns/prov#"),
        ("dcterms", "http://purl.org/dc/terms/"),
        ("rdfs", "http://www.w3.org/2000/01/rdf-schema#"),
        ("rdf", "http://www.w3.org/1999/02/22-rdf-syntax-ns#"),
        ("xsd", "http://www.w3.org/2001/XMLSchema#"),
        ("foaf", "http://xmlns.com/foaf/0.1/"),
    ]
    .into_iter()
    .chain(extra.iter().copied())
    {
        serializer = serializer
            .with_prefix(prefix, ns)
            .expect("constant prefixes are valid");
    }
    let mut writer = serializer.for_writer(Vec::new());
    for triple in triples {
        writer
            .serialize_triple(triple)
            .expect("writing to a Vec cannot fail");
    }
    String::from_utf8(writer.finish().expect("writing to a Vec cannot fail"))
        .expect("Turtle is UTF-8")
}

/// Whether any term of `triple` is a blank node.
pub fn has_blank_node(triple: &Triple) -> bool {
    matches!(triple.subject, NamedOrBlankNode::BlankNode(_))
        || matches!(triple.object, Term::BlankNode(_))
}

/// The IRI of a subject, or `None` for a blank node.
pub fn subject_iri(subject: &NamedOrBlankNode) -> Option<&str> {
    match subject {
        NamedOrBlankNode::NamedNode(n) => Some(n.as_str()),
        _ => None,
    }
}

/// The IRI of an object, or `None` for a literal or a blank node.
pub fn object_iri(object: &Term) -> Option<&str> {
    match object {
        Term::NamedNode(n) => Some(n.as_str()),
        _ => None,
    }
}
