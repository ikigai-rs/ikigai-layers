//! The fixture every suite shares: a configuration directory with three channel layers,
//! two collaborations, six participants and their certificates; a kernel binding the
//! layer space beside the real `urn:shacl:validate`, with a JSON Meta face and a clock.

#![allow(dead_code)] // each suite uses a different subset of these helpers

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Clock, Error, Fallback, Iri, Kernel, Representation, Request, Space, Time,
    Verb,
};
use ikigai_layers::Host;

/// A scratch directory, removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(label: &str) -> Scratch {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ikigai-layers-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        Scratch(dir)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    pub fn write(&self, rel: &str, text: &str) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    pub fn remove(&self, rel: &str) {
        std::fs::remove_file(self.0.join(rel)).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A clock that advances one second per reading, from 2026-10-10T00:00:00Z.
pub struct TickingClock(AtomicU64);

impl Default for TickingClock {
    fn default() -> Self {
        TickingClock(AtomicU64::new(1_791_590_400_000))
    }
}

impl Clock for TickingClock {
    fn now(&self) -> Time {
        Time::from_millis(self.0.fetch_add(1_000, Ordering::SeqCst))
    }
}

pub const IK: &str = "https://ikigai-rs.dev/ns#";

/// A machine participant named the way its door names it.
pub const GONK_FP: &str = "6f1c00000000000000000000000000000000000000000000000000000000abcd";

pub fn gonk() -> String {
    format!("urn:iki:host:layers:client:{GONK_FP}")
}

pub fn participant(slug: &str) -> String {
    format!("urn:collab:participant:{slug}")
}

/// The shapes every fixture layer uses: a message and a question, CLOSED, written the
/// way SHACL is usually written (blank property shapes, a list), so the host's
/// skolemization is exercised by every test that validates.
pub const SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<urn:example:shape:message> a sh:NodeShape ;
  sh:targetClass ik:Message ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ; sh:maxLength 4000 ] ,
              [ sh:path ik:inReplyTo ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path ik:supersedes ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .

<urn:example:shape:question> a sh:NodeShape ;
  sh:targetClass ik:Question ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path ik:body ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;

pub const CONTEXT: &str = r#"{ "@context": {
  "ik": "https://ikigai-rs.dev/ns#",
  "dcterms": "http://purl.org/dc/terms/",
  "prov": "http://www.w3.org/ns/prov#",
  "xsd": "http://www.w3.org/2001/XMLSchema#",
  "Message": "ik:Message", "Question": "ik:Question",
  "body": "ik:body",
  "inReplyTo": { "@id": "ik:inReplyTo", "@type": "@id" },
  "supersedes": { "@id": "ik:supersedes", "@type": "@id" },
  "subject": { "@id": "dcterms:subject", "@type": "@id" },
  "attributedTo": { "@id": "prov:wasAttributedTo", "@type": "@id" },
  "created": { "@id": "dcterms:created", "@type": "xsd:dateTime" },
  "seq": { "@id": "ik:seq", "@type": "xsd:integer" }
} }
"#;

pub fn participants_ttl() -> String {
    format!(
        r#"@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:collab:participant:bobby> a foaf:Person ; rdfs:label "Bobby" .
<urn:collab:participant:bobby-claude> a prov:SoftwareAgent ; prov:actedOnBehalfOf <urn:collab:participant:bobby> .
<urn:collab:participant:brian> a foaf:Person ; rdfs:label "Brian" .
<urn:collab:participant:hub> a prov:SoftwareAgent ; prov:actedOnBehalfOf <urn:collab:participant:brian> .
<urn:collab:participant:chris> a foaf:Person ; rdfs:label "Chris" .
<{gonk}> a ik:Machine ; rdfs:label "plasma gonk" ; prov:actedOnBehalfOf <urn:collab:participant:brian> .
"#,
        gonk = gonk()
    )
}

pub const BOBBY_TOML: &str = r#"label = "Bobby"

[[mount]]
layer = "talk"
bobby        = ["read", "feed", "say"]
bobby-claude = ["read", "feed", "say"]
brian        = ["read", "feed", "say"]
hub          = ["read", "feed", "say"]

[[mount]]
layer = "talk-bobby"
bobby = ["read", "feed", "say"]
brian = ["read", "feed", "say"]
"urn:iki:host:layers:client:6f1c00000000000000000000000000000000000000000000000000000000abcd" = ["read", "feed"]
"#;

pub const CHRIS_TOML: &str = r#"[[mount]]
layer = "talk"
chris = ["read", "feed", "say"]
brian = ["read", "feed", "say"]

[[mount]]
layer = "notes"
chris = ["read", "feed", "say"]
brian = ["read"]
"#;

pub const ENROLLMENT_TOML: &str = r#"bobby = "AA:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:01"
bobby-claude = ["aa00000000000000000000000000000000000000000000000000000000000002"]
brian = "aa00000000000000000000000000000000000000000000000000000000000003"
chris = "aa00000000000000000000000000000000000000000000000000000000000005"
"#;

/// Write the fixture configuration into `dir`: layers `talk` (shared by both
/// collaborations), `talk-bobby` (bobby's alone) and `notes` (chris's alone).
pub fn write_config(dir: &Scratch) {
    dir.write("layers/msg.shapes.ttl", SHAPES);
    dir.write("layers/msg.context.jsonld", CONTEXT);
    dir.write(
        "layers/talk.toml",
        "model = \"channel\"\nlabel = \"Talk\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\nshared = [\"bobby\", \"chris\"]\n",
    );
    dir.write(
        "layers/talk-bobby.toml",
        "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\nentry_max = \"2KiB\"\n",
    );
    dir.write(
        "layers/notes.toml",
        "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\n",
    );
    dir.write("participants.ttl", &participants_ttl());
    dir.write("collabs/bobby.toml", BOBBY_TOML);
    dir.write("collabs/chris.toml", CHRIS_TOML);
    dir.write("enrollment.toml", ENROLLMENT_TOML);
}

/// A host over a fresh fixture: its config and state directories, and a kernel.
pub struct Fixture {
    pub config: Scratch,
    pub state: Scratch,
    pub host: Arc<Host>,
    pub kernel: Kernel,
}

pub fn kernel_over(host: &Arc<Host>, with_validator: bool) -> Kernel {
    let mut spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_layers::space(host))];
    if with_validator {
        spaces.push(Arc::new(ikigai_shacl::space()));
    }
    Kernel::with_meta_renderer(
        Arc::new(Fallback::new(spaces)),
        Arc::new(ikigai_vocab::TurtleRenderer),
    )
    .with_clock(Arc::new(TickingClock::default()))
}

pub fn fixture() -> Fixture {
    let config = Scratch::new("config");
    write_config(&config);
    let state = Scratch::new("state");
    let host = Host::open(config.path(), state.path()).expect("the fixture configuration");
    let kernel = kernel_over(&host, true);
    Fixture {
        config,
        state,
        host,
        kernel,
    }
}

impl Fixture {
    /// Open the host again over the same directories (a restart).
    pub fn reopen(&mut self) {
        self.host = Host::open(self.config.path(), self.state.path()).expect("reopen");
        self.kernel = kernel_over(&self.host, true);
    }
}

/// The capability a door mints for `principal` holding `scopes`.
pub fn cap(principal: &str, scopes: &[&str]) -> Capability {
    Capability::scoped(scopes.iter().map(|s| s.to_string()))
        .with_principal(principal)
        .expect("a principal")
}

/// Everything a full participant of every fixture layer holds.
pub fn member(principal: &str) -> Capability {
    let mut scopes = Vec::new();
    for layer in ["talk", "talk-bobby", "notes"] {
        for verb in ["read", "feed", "say"] {
            scopes.push(format!("urn:cap:layer:{verb}:{layer}"));
        }
    }
    Capability::scoped(scopes)
        .with_principal(principal)
        .expect("a principal")
}

pub fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("an IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

pub fn issue(
    kernel: &Kernel,
    verb: Verb,
    iri: &str,
    args: &[(&str, &str)],
    capability: &Capability,
) -> Result<Representation, Error> {
    block_on(kernel.issue(request(verb, iri, args), capability))
}

pub fn text(repr: &Representation) -> String {
    String::from_utf8(repr.bytes.clone()).expect("UTF-8")
}

pub fn source(
    kernel: &Kernel,
    iri: &str,
    args: &[(&str, &str)],
    capability: &Capability,
) -> String {
    text(
        &issue(kernel, Verb::Source, iri, args, capability)
            .unwrap_or_else(|e| panic!("{iri}: {e}")),
    )
}

/// A message, as an author writes it.
pub fn message(body: &str) -> String {
    format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message ; ik:body \"{body}\" .\n")
}

/// `say` `content` into `layer` as `principal` (a full member), answering the entry IRI.
pub fn say(kernel: &Kernel, layer: &str, principal: &str, content: &str) -> Result<String, Error> {
    issue(
        kernel,
        Verb::Sink,
        &format!("urn:layer:{layer}:say"),
        &[("content", content)],
        &member(principal),
    )
    .map(|r| text(&r))
}

/// The triples of a Turtle document, as N-Triples-ish strings, for assertions.
pub fn triples(turtle: &str) -> Vec<oxrdf::Triple> {
    ikigai_layers::rdf::parse_turtle(turtle).expect("Turtle")
}
