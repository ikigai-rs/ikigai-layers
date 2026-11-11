//! The public join (ledger #3290): public read-only layers, the host-answered lobby, and its
//! guards. Each certificate is a well-formed DER document built here, and the host's key is
//! generated from a seed the test picks.

#![cfg(feature = "host")]

mod common;

use std::process::Command;
use std::sync::Arc;

use base64::Engine as _;
use common::*;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::spki::EncodePublicKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Clock, Description, Endpoint, EndpointSpace, Error, Exact, Fallback,
    Invocation, Iri, Kernel, ReprType, Representation, Request, Space, Time, Verb,
};
use ikigai_layers::grants::{generate, load_fingerprints};
use ikigai_layers::onboard::{fingerprint_of_pem, signing_bytes_of};
use ikigai_layers::{Config, Host};
use sha2::Digest as _;

const DOOR: &str = "edge";
const SAY: &str = "urn:layer:join:say";
const RELEASES_ENTRIES: &str = "urn:layer:releases:entries";
/// The instant [`TickingClock`] starts from.
const T0: u64 = 1_791_590_400_000;
const MINUTE: u64 = 60 * 1000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const ADDRESS: &str = "198.51.100.7:4433";
const HOST_SEED: [u8; 32] = [0x33; 32];

const FEED_SHAPES: &str = r#"@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .

<urn:example:shape:announcement> a sh:NodeShape ;
  sh:targetClass ik:Message ;
  sh:closed true ;
  sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path dcterms:title ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ,
              [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
              [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
              [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
"#;
const FEED_CONTEXT: &str =
    r#"{ "@context": { "dcterms": "http://purl.org/dc/terms/", "title": "dcterms:title" } }"#;
const SCHEME: &str = r#"@prefix skos: <http://www.w3.org/2004/02/skos/core#> .
<urn:example:scheme:releases> a skos:ConceptScheme .
<urn:example:concept:release> a skos:Concept ; skos:prefLabel "Release" ; skos:notation "1" .
"#;

const FEED: &str = "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\nfeed = true\npublishers = [\"brian\"]\nscheme = \"feed.scheme.ttl\"\n";

fn host_key_pem() -> String {
    SigningKey::from_bytes(&HOST_SEED)
        .to_pkcs8_pem(LineEnding::LF)
        .expect("a PKCS8 PEM")
        .to_string()
}

/// A config directory: the fixture's layers, two public feeds, a private feed, the example join
/// lobby, and a `[join]` table holding `extra`.
fn join_config(extra: &str) -> Scratch {
    let config = Scratch::new("join");
    write_config(&config);
    config.write("layers/feed.shapes.ttl", FEED_SHAPES);
    config.write("layers/feed.context.jsonld", FEED_CONTEXT);
    config.write("layers/feed.scheme.ttl", SCHEME);
    config.write(
        "layers/releases.toml",
        &format!("{FEED}label = \"Releases\"\npublic = true\n"),
    );
    config.write("layers/features.toml", &format!("{FEED}public = true\n"));
    config.write("layers/internal.toml", FEED);
    config.write(
        "layers/join.toml",
        include_str!("../onboard/layers/join.toml"),
    );
    config.write(
        "layers/join.shapes.ttl",
        include_str!("../onboard/layers/join.shapes.ttl"),
    );
    config.write(
        "layers/join.context.jsonld",
        include_str!("../onboard/layers/join.context.jsonld"),
    );
    config.write("join.pem", &host_key_pem());
    config.write(
        "layers.toml",
        &format!("[join]\nhost_key = \"join.pem\"\n{extra}\n"),
    );
    config
}

struct Door {
    config: Scratch,
    state: Scratch,
    host: Arc<Host>,
    kernel: Kernel,
}

fn door_with(extra: &str) -> Door {
    let config = join_config(extra);
    let state = Scratch::new("join-state");
    let host = Host::open(config.path(), state.path(), &key(config.path())).expect("the host");
    let kernel = kernel_over(&host);
    Door {
        config,
        state,
        host,
        kernel,
    }
}

fn door() -> Door {
    door_with("")
}

impl Door {
    /// A restart: the old host goes first, it holds the state directory's lock.
    fn restart(&mut self) {
        let between = Scratch::new("between");
        let empty = Host::open(
            between.path(),
            between.path().join("state"),
            &key(between.path()),
        )
        .expect("empty");
        self.kernel = kernel_over(&empty);
        self.host = empty;
        self.host = Host::open(
            self.config.path(),
            self.state.path(),
            &key(self.config.path()),
        )
        .expect("reopen");
        self.kernel = kernel_over(&self.host);
    }

    fn at(&self, millis: u64) -> Kernel {
        Kernel::with_meta_renderer(
            root_over(&self.host),
            Arc::new(ikigai_vocab::TurtleRenderer),
        )
        .with_clock(Arc::new(Fixed(millis)))
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.config.path().join(rel)).ok()
    }

    /// What a door mints for a stranger's or a joined certificate, at the kernel clock's start.
    fn minted(&self, fp: &str) -> Capability {
        self.minted_at(fp, None, T0).expect("minted")
    }

    fn minted_at(&self, fp: &str, source: Option<&str>, millis: u64) -> Option<Capability> {
        self.host
            .door_capability(DOOR, fp, source, Time::from_millis(millis))
            .expect("the door mints")
    }

    fn deny(&self, fp: &str) {
        self.config.write("deny.txt", &format!("# abuse\n{fp}\n"));
    }
}

struct Fixed(u64);

impl Clock for Fixed {
    fn now(&self) -> Time {
        Time::from_millis(self.0)
    }
}

/// One certificate: a SEQUENCE holding an OCTET STRING, so the host's PEM and DER checks pass.
fn certificate(name: &str) -> (String, String) {
    let body = format!("a test certificate for {name}").into_bytes();
    let mut der = vec![0x30, (body.len() + 2) as u8, 0x04, body.len() as u8];
    der.extend(body);
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let pem = format!("-----BEGIN CERTIFICATE-----\n{b64}\n-----END CERTIFICATE-----\n");
    let fp = fingerprint_of_pem(&pem).unwrap();
    (pem, fp)
}

fn literal(value: &str) -> String {
    oxrdf::Literal::new_simple_literal(value).to_string()
}

/// A request, as the Python client writes it.
fn request_turtle(pem: &str, fp: &str, layers: &[&str]) -> String {
    let layers = layers
        .iter()
        .map(|l| format!("<urn:layer:{l}>"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .\n<urn:iki:onboard:join:{fp}> a onb:JoinRequest ;\n  \
         onb:certificate {} ;\n  onb:fingerprint {} ;\n  onb:layer {layers} .\n",
        literal(pem),
        literal(fp),
    )
}

fn join(d: &Door, name: &str, layers: &[&str]) -> Result<serde_json::Value, Error> {
    let (pem, fp) = certificate(name);
    join_as(d, &pem, &fp, layers)
}

fn join_as(d: &Door, pem: &str, fp: &str, layers: &[&str]) -> Result<serde_json::Value, Error> {
    issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[
            ("content", &request_turtle(pem, fp, layers)),
            ("as", "application/json"),
        ],
        &d.minted(fp),
    )
    .map(|r| serde_json::from_str(&text(&r)).expect("a JSON answer"))
}

fn read(d: &Door, cap: &Capability) -> Result<String, Error> {
    issue(&d.kernel, Verb::Source, RELEASES_ENTRIES, &[], cap).map(|r| text(&r))
}

fn publish(d: &Door) {
    let brian = cap(&participant("brian"), &["urn:cap:layer:say:releases"]);
    let content = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n\
                   <urn:layer:new> a ik:Message ; dcterms:title \"A release\" .\n";
    issue(
        &d.kernel,
        Verb::Sink,
        "urn:layer:releases:say",
        &[("content", content)],
        &brian,
    )
    .expect("a publisher says");
}

/// What a capability holds beyond naming its principal.
fn scopes(cap: &Capability) -> Vec<String> {
    let named = ikigai_core::principal_scope(cap.principal().unwrap()).unwrap();
    cap.scopes()
        .expect("a scoped capability")
        .iter()
        .filter(|s| **s != named)
        .cloned()
        .collect()
}

fn refused_opening(d: &str, toml_extra: &str) -> String {
    let config = join_config("");
    config.write("layers/releases.toml", d);
    if !toml_extra.is_empty() {
        config.write("layers.toml", toml_extra);
    }
    let state = Scratch::new("refused");
    Host::open(config.path(), state.path(), &key(config.path()))
        .err()
        .map(|e| e.to_string())
        .expect("the host refuses")
}

// 1. `public`

#[test]
fn only_a_feed_may_be_public() {
    let err = refused_opening(
        "model = \"channel\"\nshapes = \"feed.shapes.ttl\"\ncontext = \"feed.context.jsonld\"\npublic = true\n",
        "",
    );
    assert!(err.contains("`public` is a feed option"), "{err}");
}

#[test]
fn a_public_feed_is_read_by_a_principal_that_is_no_participant() {
    let d = door();
    publish(&d);
    let reader = cap(
        &format!("urn:iki:host:{DOOR}:client:{}", "ab".repeat(32)),
        &["urn:cap:layer:read:releases"],
    );
    assert!(read(&d, &reader).unwrap().contains("A release"));
}

// 2. The lobby's definition, and proof of possession.

#[test]
fn join_is_a_lobbys_option_and_public_the_only_value() {
    let lobby =
        "model = \"channel\"\nshapes = \"join.shapes.ttl\"\ncontext = \"join.context.jsonld\"\n";
    let cases: [(String, &str); 3] = [
        (
            format!("{lobby}join = \"public\"\n"),
            "does not say `lobby = true`",
        ),
        (
            format!("{lobby}lobby = true\njoin = \"everyone\"\n"),
            "`join` is `everyone`",
        ),
        (
            "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\nlobby = true\njoin = \"public\"\n".to_string(),
            "a join lobby's shapes target one class",
        ),
    ];
    for (definition, expected) in cases {
        let config = join_config("");
        config.write("layers/join.toml", &definition);
        let state = Scratch::new("refused");
        let err = Host::open(config.path(), state.path(), &key(config.path()))
            .err()
            .map(|e| e.to_string())
            .expect("the host refuses");
        assert!(err.contains(expected), "{expected} in {err}");
    }
}

#[test]
fn a_join_lobby_needs_a_join_table_and_a_key_that_signs() {
    let config = join_config("");
    config.write("layers.toml", "");
    let state = Scratch::new("refused");
    let err = Host::open(config.path(), state.path(), &key(config.path()))
        .err()
        .map(|e| e.to_string())
        .unwrap();
    assert!(err.contains("has no `[join]` table"), "{err}");

    let config = join_config("");
    config.remove("layers/join.toml");
    let state = Scratch::new("refused");
    let err = Host::open(config.path(), state.path(), &key(config.path()))
        .err()
        .map(|e| e.to_string())
        .unwrap();
    assert!(err.contains("no layer says `join"), "{err}");

    let config = join_config("");
    config.write("join.pem", "not a key\n");
    let state = Scratch::new("refused");
    let err = Host::open(config.path(), state.path(), &key(config.path()))
        .err()
        .map(|e| e.to_string())
        .unwrap();
    assert!(err.contains("`join.host_key` cannot sign"), "{err}");
}

#[test]
fn the_poster_holds_the_certificate_it_states() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let (other_pem, other_fp) = certificate("mallory");

    // A posted certificate that is not the poster's: its fingerprint is another's.
    let wrong = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[(
            "content",
            &request_turtle(&other_pem, &other_fp, &["releases"]),
        )],
        &d.minted(&fp),
    );
    match wrong {
        Err(Error::Denied(m)) => assert!(m.contains("for itself only"), "{m}"),
        other => panic!("{other:?}"),
    }

    // The right fingerprint and someone else's certificate.
    let swapped = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&other_pem, &fp, &["releases"]))],
        &d.minted(&fp),
    );
    match swapped {
        Err(Error::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("hashes to"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    assert!(d.read(&format!("joined/identities/{fp}.json")).is_none());
    let _ = pem;
}

// 3. The answer.

#[test]
fn a_join_is_answered_synchronously_with_the_granted_layers() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let answer = join_as(&d, &pem, &fp, &["features", "releases"]).expect("joined");

    let mut granted: Vec<String> = answer["granted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap().to_string())
        .collect();
    granted.sort();
    assert_eq!(
        granted,
        [
            "urn:cap:layer:feed:features",
            "urn:cap:layer:feed:releases",
            "urn:cap:layer:read:features",
            "urn:cap:layer:read:releases",
        ]
    );
    let principal = format!("urn:iki:host:{DOOR}:client:{fp}");
    assert_eq!(answer["principal"], principal.as_str());
    assert_eq!(
        answer["layers"],
        serde_json::json!(["urn:layer:features", "urn:layer:releases"])
    );
    let entitlement = answer["entitlement"].as_str().unwrap();
    assert!(
        entitlement.starts_with(&format!("urn:iki:host:{DOOR}:entitlement:")),
        "{entitlement}"
    );
    assert!(answer["expires"].as_str().is_some(), "{answer}");

    // The grant is live on the joiner's next connection.
    let next = d.minted(&fp);
    assert_eq!(next.principal(), Some(principal.as_str()));
    assert_eq!(
        scopes(&next),
        [
            "urn:cap:layer:feed:features",
            "urn:cap:layer:feed:releases",
            "urn:cap:layer:read:features",
            "urn:cap:layer:read:releases",
        ]
    );
    publish(&d);
    assert!(read(&d, &next).unwrap().contains("A release"));
}

#[test]
fn the_plain_answer_labels_each_line() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let answer = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&pem, &fp, &["releases"]))],
        &d.minted(&fp),
    )
    .expect("joined");
    let body = text(&answer);
    assert!(body.starts_with("entitlement urn:iki:host:"), "{body}");
    assert!(body.contains("\nprincipal urn:iki:host:"), "{body}");
    assert!(body.contains("\nexpires 20"), "{body}");
    assert!(
        body.contains("\ngranted urn:cap:layer:read:releases"),
        "{body}"
    );
}

/// The entitlement is the host's, and verifies under the host's public key.
#[test]
fn the_entitlement_is_host_signed() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let answer = join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let iri = answer["entitlement"].as_str().unwrap();
    let turtle = d
        .read(&format!("joined/identities/{fp}.ttl"))
        .expect("the entitlement");
    for term in [
        "Entitlement",
        "onboard#principal",
        "onboard#expires",
        "onboard#request",
        "urn:cap:layer:read:releases",
        "urn:cap:layer:feed:releases",
    ] {
        assert!(turtle.contains(term), "{term} in {turtle}");
    }
    assert!(!turtle.contains("urn:collab:participant"), "{turtle}");

    let sig = triples(&turtle)
        .into_iter()
        .find(|t| t.predicate.as_str().ends_with("#signature"))
        .map(|t| match t.object {
            oxrdf::Term::Literal(l) => l.value().to_string(),
            other => panic!("{other}"),
        })
        .expect("a signature");
    let bytes = signing_bytes_of(&turtle, iri).unwrap();
    let public = SigningKey::from_bytes(&HOST_SEED)
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let keys = EndpointSpace::new().bind(Exact::new("urn:test:key"), Pem(public.into_bytes()));
    let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_sign::space()), Arc::new(keys)];
    let verifier = Kernel::new(Arc::new(Fallback::new(spaces)));
    let request = Request::new(Verb::Source, Iri::parse("urn:sign:verify").unwrap())
        .with_arg("in", ArgRef::Inline(bytes))
        .with_arg("sig", ArgRef::Inline(sig.into_bytes()))
        .with_arg("key", ArgRef::Inline(b"urn:test:key".to_vec()));
    let verdict = block_on(verifier.issue(request, &Capability::scoped(Vec::<String>::new())))
        .expect("verified");
    let verdict = String::from_utf8(verdict.bytes).unwrap();
    assert!(verdict.starts_with("valid:"), "{verdict}");
}

struct Pem(Vec<u8>);

#[async_trait::async_trait]
impl Endpoint for Pem {
    async fn invoke(&self, _inv: &Invocation<'_>) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/x-pem-file"),
            self.0.clone(),
        ))
    }

    fn describe(&self) -> Description {
        Description::new("test-key").verb(Verb::Source)
    }
}

#[test]
fn a_request_naming_a_non_public_layer_gets_nothing() {
    let d = door();
    let (pem, fp) = certificate("alice");
    for bad in ["internal", "talk", "no-such-layer"] {
        match join_as(&d, &pem, &fp, &["releases", bad]) {
            Err(Error::Denied(m)) => {
                assert!(
                    m.contains(&format!("layer `{bad}` is not a public layer")),
                    "{m}"
                );
                assert!(m.contains("nothing in this request was granted"), "{m}");
            }
            other => panic!("{bad}: {other:?}"),
        }
    }
    assert!(d.read(&format!("joined/identities/{fp}.json")).is_none());
    // Still a stranger: say on the lobby, and nothing else.
    assert_eq!(scopes(&d.minted(&fp)), ["urn:cap:layer:say:join"]);
}

#[test]
fn a_private_layer_and_a_missing_one_are_refused_in_the_same_words() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let message = |layer: &str| match join_as(&d, &pem, &fp, &[layer]) {
        Err(Error::Denied(m)) => m,
        other => panic!("{layer}: {other:?}"),
    };
    assert_eq!(
        message("internal").replace("internal", "X"),
        message("absent").replace("absent", "X")
    );
}

#[test]
fn a_request_with_no_layer_is_refused() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let body = format!(
        "@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .\n<urn:layer:new> a onb:JoinRequest ;\n  \
         onb:certificate {} ;\n  onb:fingerprint {} .\n",
        literal(&pem),
        literal(&fp),
    );
    let refused = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &body)],
        &d.minted(&fp),
    );
    match refused {
        Err(Error::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("one or more layers"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
}

// 4. What a joined identity holds.

#[test]
fn a_joined_identitys_say_anywhere_is_refused() {
    let d = door();
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let joined = d.minted(&fp);
    for layer in ["releases", "talk", "join"] {
        let content = if layer == "join" {
            request_turtle(&pem, &fp, &["features"])
        } else {
            message("nope")
        };
        let refused = issue(
            &d.kernel,
            Verb::Sink,
            &format!("urn:layer:{layer}:say"),
            &[("content", &content)],
            &joined,
        );
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{layer}: {refused:?}"
        );
    }
    // A door that also gave it the lobby's say: the host still refuses a second join.
    let both = cap(
        joined.principal().unwrap(),
        &["urn:cap:layer:say:join", "urn:cap:layer:read:releases"],
    );
    let refused = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&pem, &fp, &["features"]))],
        &both,
    );
    match refused {
        Err(Error::Denied(m)) => assert!(m.contains("has already joined"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_joined_identity_is_in_no_collaboration() {
    let d = door();
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let joined = d.minted(&fp);

    // It holds read and feed on the public layer and nothing else, so urn:layers:me, which a
    // holder of any collaboration token may read, is refused it: there is nothing to list.
    assert_eq!(
        scopes(&joined),
        ["urn:cap:layer:feed:releases", "urn:cap:layer:read:releases"]
    );
    let refused = issue(
        &d.kernel,
        Verb::Source,
        "urn:layers:me",
        &[("as", "application/json")],
        &joined,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");

    // Handed a collaboration token, its name still lists no collaboration.
    let more = cap(
        joined.principal().unwrap(),
        &["urn:cap:collab:read:bobby", "urn:cap:layer:read:releases"],
    );
    let me = source(
        &d.kernel,
        "urn:layers:me",
        &[("as", "application/json")],
        &more,
    );
    let me: serde_json::Value = serde_json::from_str(&me).unwrap();
    assert_eq!(me["collaborations"], serde_json::json!([]), "{me}");
    assert_eq!(me["participant"], serde_json::Value::Null, "{me}");
}

// 5. State.

#[test]
fn a_joiner_lives_in_joined_and_never_in_the_operators_files() {
    let mut d = door();
    let before = [
        d.read("participants.ttl"),
        d.read("grants.json"),
        d.read("clients.json"),
    ];
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();

    assert_eq!(
        before,
        [
            d.read("participants.ttl"),
            d.read("grants.json"),
            d.read("clients.json"),
        ],
        "a join edits nothing of the operator's"
    );
    assert!(!d.read("participants.ttl").unwrap().contains(&fp));

    // The door's two files for the joined set, beside the operator's.
    let clients: serde_json::Value =
        serde_json::from_str(&d.read("joined/clients.json").unwrap()).unwrap();
    let principal = format!("urn:iki:host:{DOOR}:client:{fp}");
    assert_eq!(clients["clients"][&fp]["principal"], principal.as_str());
    let grant = clients["clients"][&fp]["grant"]
        .as_str()
        .unwrap()
        .to_string();
    let grants: serde_json::Value =
        serde_json::from_str(&d.read("joined/grants.json").unwrap()).unwrap();
    assert_eq!(
        grants[&grant],
        serde_json::json!(["urn:cap:layer:feed:releases", "urn:cap:layer:read:releases"])
    );
    assert!(d.read(&format!("joined/clients/{fp}.crt")).is_some());

    // An operator re-applying over its own files leaves the joiner alone.
    d.config.write("participants.ttl", &participants_ttl());
    d.restart();
    assert_eq!(
        scopes(&d.minted(&fp)).len(),
        2,
        "a restart keeps the joiner"
    );
    assert!(d.read(&format!("joined/identities/{fp}.json")).is_some());
}

#[test]
fn a_joined_certificate_is_trusted_only_while_it_is_live() {
    let d = door();
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    assert!(d.read(&format!("joined/clients/{fp}.crt")).is_some());
    assert!(d.minted_at(&fp, None, T0 + 91 * DAY).is_some());
    assert_eq!(
        d.read(&format!("joined/clients/{fp}.crt")),
        None,
        "the certificate leaves the trust set"
    );
    let clients = d.read("joined/clients.json").unwrap();
    assert!(!clients.contains(&fp), "{clients}");
}

// 4 again: the deny list.

#[test]
fn a_denied_fingerprint_is_refused_at_join() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let stranger = d.minted(&fp);
    d.deny(&fp);
    let refused = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&pem, &fp, &["releases"]))],
        &stranger,
    );
    match refused {
        Err(Error::Denied(m)) => assert!(m.contains("deny list"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(d.read(&format!("joined/identities/{fp}.json")).is_none());
}

#[test]
fn a_denied_fingerprint_is_refused_at_connect_and_on_a_live_connection() {
    let d = door();
    publish(&d);
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let held = d.minted(&fp);
    assert!(read(&d, &held).is_ok());

    d.deny(&fp);
    assert!(d.minted_at(&fp, None, T0).is_none(), "refused at connect");
    let refused = read(&d, &held);
    match refused {
        Err(Error::Denied(m)) => assert!(m.contains("deny list"), "{m}"),
        other => panic!("{other:?}"),
    }

    // A stranger on the list gets no lobby either, and an entry off it connects again.
    let (_, other) = certificate("mallory");
    d.deny(&other);
    assert!(d.minted_at(&other, None, T0).is_none());
    d.config.write("deny.txt", "");
    assert!(d.minted_at(&fp, None, T0).is_some());
}

#[test]
fn a_deny_list_that_does_not_read_stops_joins_and_says_where() {
    let d = door();
    let (pem, fp) = certificate("alice");
    let stranger = d.minted(&fp);
    d.config.write("deny.txt", "not a fingerprint\n");
    let refused = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&pem, &fp, &["releases"]))],
        &stranger,
    );
    match refused {
        Err(Error::Unavailable(m)) => assert!(m.contains("deny.txt line 1"), "{m}"),
        other => panic!("{other:?}"),
    }
    let connect = d
        .host
        .door_capability(DOOR, &fp, None, Time::from_millis(T0));
    assert!(connect.unwrap_err().contains("deny.txt line 1"));
}

// 4 again: the limits, each surviving a restart.

fn unavailable(answer: Result<serde_json::Value, Error>, expected: &str) {
    match answer {
        Err(Error::Unavailable(m)) => assert!(m.contains(expected), "{expected} in {m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_per_certificate_limit_survives_a_restart() {
    let mut d = door_with("per_certificate = 2\n");
    let (pem, fp) = certificate("alice");
    for _ in 0..2 {
        // A refusal for a private layer counts: a stranger's probing is not free.
        assert!(join_as(&d, &pem, &fp, &["internal"]).is_err());
    }
    unavailable(
        join_as(&d, &pem, &fp, &["releases"]),
        "at most 2 requests an hour from one certificate",
    );
    d.restart();
    unavailable(
        join_as(&d, &pem, &fp, &["releases"]),
        "at most 2 requests an hour from one certificate",
    );
}

#[test]
fn a_restart_starts_a_fresh_key_so_the_per_address_count_begins_again() {
    let mut d = door_with("per_address = 2\n");
    let addr = Some(ADDRESS);
    for name in ["alice", "bobby"] {
        let (pem, fp) = certificate(name);
        d.minted_at(&fp, addr, T0).expect("a lobby");
        join_as(&d, &pem, &fp, &["releases"]).expect("joined");
    }
    let (pem, fp) = certificate("carol");
    d.minted_at(&fp, addr, T0).expect("a lobby");
    unavailable(
        join_as(&d, &pem, &fp, &["releases"]),
        "from one source address",
    );

    // The key was only ever in memory, so what the old process hashed matches nothing now.
    d.restart();
    assert!(
        limits(&d).iter().all(|a| a.get("source").is_none()),
        "a hash outlived its key: {:?}",
        limits(&d)
    );
    d.minted_at(&fp, addr, T0).expect("a lobby");
    join_as(&d, &pem, &fp, &["releases"]).expect("a fresh key counts from nothing");
}

#[test]
fn the_global_cap_survives_a_restart() {
    let mut d = door_with("per_hour = 2\n");
    for name in ["alice", "bobby"] {
        join(&d, name, &["releases"]).expect("joined");
    }
    unavailable(
        join(&d, "carol", &["releases"]),
        "has taken its 2 requests this hour",
    );
    d.restart();
    unavailable(
        join(&d, "carol", &["releases"]),
        "has taken its 2 requests this hour",
    );
    // The window is an hour.
    let (pem, fp) = certificate("dana");
    let later = d.at(T0 + 61 * 60 * 1000);
    let joined = issue(
        &later,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&pem, &fp, &["releases"]))],
        &d.minted(&fp),
    );
    assert!(joined.is_ok(), "{joined:?}");
}

#[test]
fn the_rates_are_sealed_at_rest() {
    let d = door();
    let (pem, fp) = certificate("alice");
    d.minted_at(&fp, Some("198.51.100.7:4433"), T0)
        .expect("a lobby");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let bytes = std::fs::read(
        d.state
            .path()
            .join(ikigai_layers::onboard::join::LIMITS_FILE),
    )
    .unwrap();
    assert!(bytes.starts_with(b"age-encryption.org/v1"));
    assert!(!String::from_utf8_lossy(&bytes).contains("198.51.100.7"));
}

/// The limits file as the host wrote it, opened with the state key it is sealed to.
fn limits_text(d: &Door) -> String {
    let bytes = std::fs::read(
        d.state
            .path()
            .join(ikigai_layers::onboard::join::LIMITS_FILE),
    )
    .unwrap();
    let keys = std::fs::read_to_string(d.config.path().join("state.key")).unwrap();
    let identity: age::x25519::Identity = keys
        .lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .parse()
        .unwrap();
    String::from_utf8(age::decrypt(&identity, &bytes).unwrap()).unwrap()
}

fn limits(d: &Door) -> Vec<serde_json::Value> {
    serde_json::from_str(&limits_text(d)).expect("a list of attempts")
}

/// A join request made at `millis`, by a certificate the door saw at that moment from `source`.
fn join_at(
    d: &Door,
    name: &str,
    source: Option<&str>,
    millis: u64,
) -> Result<serde_json::Value, Error> {
    let (pem, fp) = certificate(name);
    d.minted_at(&fp, source, millis).expect("a lobby");
    issue(
        &d.at(millis),
        Verb::Sink,
        SAY,
        &[
            ("content", &request_turtle(&pem, &fp, &["releases"])),
            ("as", "application/json"),
        ],
        &d.minted(&fp),
    )
    .map(|r| serde_json::from_str(&text(&r)).expect("a JSON answer"))
}

#[test]
fn the_limits_file_holds_no_address_in_any_form() {
    let d = door();
    let (pem, fp) = certificate("alice");
    d.minted_at(&fp, Some(ADDRESS), T0).expect("a lobby");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();

    let text = limits_text(&d);
    let base64 = &base64::engine::general_purpose::STANDARD;
    let bare = ADDRESS.split(':').next().unwrap();
    for form in [ADDRESS, bare] {
        let hex: String = form.bytes().map(|b| format!("{b:02x}")).collect();
        for spelled in [form.to_string(), hex, base64.encode(form)] {
            assert!(!text.contains(&spelled), "`{spelled}` is in {text}");
        }
        // Not even a hash anyone could recompute: it takes the key that was never written.
        let plain: String = sha2::Sha256::digest(form.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(!text.contains(&plain), "an unkeyed hash is in {text}");
    }
    let attempts = limits(&d);
    assert_eq!(attempts.len(), 1, "{text}");
    let attempt = attempts[0].as_object().unwrap();
    let mut members: Vec<&str> = attempt.keys().map(String::as_str).collect();
    members.sort();
    assert_eq!(members, ["at", "fingerprint", "source"], "{text}");
    let hash = attempt["source"].as_str().unwrap();
    assert!(
        hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
        "`{hash}` is not an HMAC-SHA256 in hex"
    );
}

#[test]
fn the_per_address_limit_refuses_the_eleventh_attempt_within_the_hour() {
    let d = door_with("per_address = 10\n");
    for n in 0..10 {
        join_at(&d, &format!("from-one-address-{n}"), Some(ADDRESS), T0 + n)
            .expect("within the limit");
    }
    unavailable(
        join_at(&d, "the-eleventh", Some(ADDRESS), T0 + 59 * MINUTE),
        "at most 10 requests an hour from one source address",
    );
    // The hash separates addresses as the address did.
    join_at(
        &d,
        "another-address",
        Some("198.51.100.8:4433"),
        T0 + 59 * MINUTE,
    )
    .expect("another address is another count");
}

#[test]
fn two_addresses_in_one_ipv6_64_share_the_per_address_limit() {
    let d = door_with("per_address = 1\n");
    join_at(&d, "first-in-the-64", Some("[2001:db8:1:2::1]:4433"), T0).expect("within the limit");
    unavailable(
        join_at(
            &d,
            "second-in-the-64",
            Some("[2001:db8:1:2:ffff::9]:4433"),
            T0,
        ),
        "from one source address",
    );
    join_at(&d, "next-64", Some("[2001:db8:1:3::1]:4433"), T0)
        .expect("another /64 is another count");
}

#[test]
fn the_refusal_says_how_the_address_is_counted_and_names_no_address() {
    let d = door_with("per_address = 1\nipv6_prefix = 56\n");
    join_at(&d, "first", Some("[2001:db8:1:2::1]:4433"), T0).expect("within the limit");
    let err = join_at(&d, "second", Some("[2001:db8:1:2::2]:4433"), T0)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("counting an IPv6 /56 network as one address"),
        "{err}"
    );
    assert!(!err.contains("2001:db8"), "{err}");
}

#[test]
fn ipv6_prefix_moves_the_boundary_of_one_address() {
    let d = door_with("per_address = 1\nipv6_prefix = 48\n");
    join_at(&d, "first", Some("[2001:db8:1:2::1]:4433"), T0).expect("within the limit");
    unavailable(
        join_at(&d, "same-48", Some("[2001:db8:1:ffff::1]:4433"), T0),
        "from one source address",
    );
    join_at(&d, "next-48", Some("[2001:db8:2::1]:4433"), T0).expect("another /48");

    let d = door_with("per_address = 1\nipv6_prefix = 128\n");
    join_at(&d, "first", Some("[2001:db8:1:2::1]:4433"), T0).expect("within the limit");
    join_at(&d, "next-address", Some("[2001:db8:1:2::2]:4433"), T0)
        .expect("at 128 each full address is its own count");
}

#[test]
fn a_port_is_not_part_of_the_address() {
    for (first, second) in [
        ("198.51.100.7:4433", "198.51.100.7:50000"),
        ("198.51.100.7:4433", "198.51.100.7"),
        ("[2001:db8:1:2::1]:4433", "2001:db8:1:2::1"),
    ] {
        let d = door_with("per_address = 1\n");
        join_at(&d, "first", Some(first), T0).expect("within the limit");
        unavailable(
            join_at(&d, "second", Some(second), T0),
            "from one source address",
        );
    }
}

#[test]
fn an_ipv4_mapped_address_counts_as_its_ipv4_address() {
    let d = door_with("per_address = 1\n");
    join_at(&d, "first", Some("[::ffff:198.51.100.7]:4433"), T0).expect("within the limit");
    unavailable(
        join_at(&d, "same-ipv4", Some("198.51.100.7:4433"), T0),
        "from one source address",
    );
    // All of IPv4 is inside one /64 of the mapped range, and is not one address.
    join_at(&d, "other-ipv4", Some("[::ffff:198.51.100.8]:4433"), T0)
        .expect("another IPv4 address is another count");
}

#[test]
fn a_source_that_is_not_an_ip_address_is_counted_as_given() {
    let d = door_with("per_address = 1\n");
    join_at(&d, "first", Some("relay-a"), T0).expect("within the limit");
    unavailable(
        join_at(&d, "second", Some("relay-a"), T0),
        "from one source address",
    );
    join_at(&d, "third", Some("relay-b"), T0).expect("another source is another count");
}

#[test]
fn an_ipv6_prefix_out_of_range_is_refused() {
    for bits in [0, 129] {
        let config = join_config(&format!("ipv6_prefix = {bits}\n"));
        let state = Scratch::new("refused");
        let err = Host::open(config.path(), state.path(), &key(config.path()))
            .err()
            .map(|e| e.to_string())
            .unwrap();
        assert!(
            err.contains(&format!("`join.ipv6_prefix` is {bits}")),
            "{err}"
        );
    }
}

#[test]
fn a_rotated_key_no_longer_matches_last_hours_hash() {
    let d = door_with("per_address = 2\n");
    let at = T0 + 59 * MINUTE;
    join_at(&d, "starts-the-key", Some("198.51.100.9:4433"), T0).expect("joined");
    for name in ["alice", "bobby"] {
        join_at(&d, name, Some(ADDRESS), at).expect("joined");
    }
    unavailable(
        join_at(&d, "carol", Some(ADDRESS), at),
        "from one source address",
    );
    let before = limits_text(&d);
    let attempts = limits(&d);
    let last_hour = attempts.last().unwrap()["source"].as_str().unwrap();
    assert_eq!(
        attempts.iter().filter(|a| a["source"] == last_hour).count(),
        2,
        "one address hashes alike within its hour: {before}"
    );

    // Alice's and Bobby's attempts are still inside the hour, so only the key's rotation can
    // let Dana in: they still count toward the global limit, and no longer toward the address.
    join_at(&d, "dana", Some(ADDRESS), T0 + 60 * MINUTE).expect("the count began again");
    let after = limits_text(&d);
    assert!(!after.contains(last_hour), "last hour's hash is in {after}");
    let attempts = limits(&d);
    let hashes: Vec<&str> = attempts
        .iter()
        .filter_map(|a| a.get("source").and_then(|s| s.as_str()))
        .collect();
    assert_eq!(hashes.len(), 1, "only this hour's hash remains: {after}");
    assert_ne!(hashes[0], last_hour, "the same address, a new key");
    assert_eq!(attempts.len(), 3, "the attempts still count: {after}");
}

#[test]
fn a_limits_file_naming_an_address_is_rewritten_without_it_at_open() {
    let mut d = door_with("per_certificate = 1\n");
    let (pem, fp) = certificate("alice");
    let legacy = format!(r#"[{{"at":{T0},"fingerprint":"{fp}","address":"{ADDRESS}"}}]"#);
    std::fs::write(
        d.state
            .path()
            .join(ikigai_layers::onboard::join::LIMITS_FILE),
        sealed(d.config.path(), legacy.as_bytes()),
    )
    .unwrap();

    d.restart();
    let text = limits_text(&d);
    assert!(!text.contains("198.51.100.7"), "{text}");
    assert_eq!(limits(&d).len(), 1, "the attempt still counts: {text}");
    unavailable(
        join_as(&d, &pem, &fp, &["releases"]),
        "from one certificate",
    );
}

#[test]
fn the_cap_on_live_identities_refuses_until_one_lapses() {
    let d = door_with("max_joined = 1\n");
    join(&d, "alice", &["releases"]).expect("joined");
    unavailable(join(&d, "bobby", &["releases"]), "`join.max_joined`");
    // The cap counts the live set: a lapsed identity frees its place.
    let (pem, fp) = certificate("carol");
    let later = d.at(T0 + 91 * DAY);
    let joined = issue(
        &later,
        Verb::Sink,
        SAY,
        &[("content", &request_turtle(&pem, &fp, &["releases"]))],
        &d.minted(&fp),
    );
    assert!(joined.is_ok(), "{joined:?}");
}

// 4 again: expiry.

#[test]
fn an_expired_identity_is_refused_and_can_join_again() {
    let d = door();
    publish(&d);
    let (pem, fp) = certificate("alice");
    let first = join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let held = d.minted(&fp);
    let later = T0 + 91 * DAY;

    // A connection already open is refused on its next read.
    let refused = issue(&d.at(later), Verb::Source, RELEASES_ENTRIES, &[], &held);
    match refused {
        Err(Error::Denied(m)) => assert!(m.contains("has lapsed"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(d.read(&format!("joined/identities/{fp}.json")).is_none());

    // The next connection is a stranger's: the lobby's say, and no read.
    let stranger = d.minted_at(&fp, None, later).expect("a lobby");
    assert_eq!(scopes(&stranger), ["urn:cap:layer:say:join"]);
    assert!(matches!(read(&d, &stranger), Err(Error::Denied(_))));

    // And it may join again, as a new entitlement.
    let second = join_as(&d, &pem, &fp, &["releases"]).unwrap();
    assert_ne!(first["entitlement"], second["entitlement"]);
    assert_eq!(scopes(&d.minted(&fp)).len(), 2);
}

#[test]
fn a_read_keeps_an_identity_live() {
    let d = door();
    publish(&d);
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let held = d.minted(&fp);
    let reading = T0 + 50 * DAY;
    issue(&d.at(reading), Verb::Source, RELEASES_ENTRIES, &[], &held).expect("a read");

    // Inside 90 days of that read, though not of the join.
    let live = d.minted_at(&fp, None, T0 + 100 * DAY).expect("live");
    assert_eq!(scopes(&live).len(), 2);
    // And the read is on disk, so a restart keeps it.
    let record: serde_json::Value =
        serde_json::from_str(&d.read(&format!("joined/identities/{fp}.json")).unwrap()).unwrap();
    assert_eq!(record["last_read"], reading);
    // 90 days after the read it has lapsed.
    assert_eq!(
        scopes(&d.minted_at(&fp, None, reading + 90 * DAY).expect("a lobby")),
        ["urn:cap:layer:say:join"]
    );
}

/// A joined identity's record, as the host keeps it.
fn record(d: &Door, fp: &str) -> serde_json::Value {
    serde_json::from_str(&d.read(&format!("joined/identities/{fp}.json")).unwrap()).unwrap()
}

/// One read by `held` at `millis`.
fn read_at(d: &Door, held: &Capability, millis: u64) {
    issue(&d.at(millis), Verb::Source, RELEASES_ENTRIES, &[], held).expect("a read");
}

#[test]
fn a_join_records_its_read_as_the_day() {
    let d = door();
    let (_, fp) = certificate("alice");
    join_at(&d, "alice", None, T0 + 5 * HOUR).unwrap();
    assert_eq!(record(&d, &fp)["last_read"], T0);
    assert_eq!(record(&d, &fp)["joined_at"], T0 + 5 * HOUR);
}

#[test]
fn two_reads_in_one_day_write_one_value() {
    let d = door();
    publish(&d);
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let held = d.minted(&fp);
    let day = T0 + 10 * DAY;
    read_at(&d, &held, day + 3 * HOUR);
    assert_eq!(record(&d, &fp)["last_read"], day);

    // A mark only a write would take away.
    let path = d.config.path().join(format!("joined/identities/{fp}.json"));
    let marked = format!("{}\n", std::fs::read_to_string(&path).unwrap());
    std::fs::write(&path, &marked).unwrap();
    read_at(&d, &held, day + 9 * HOUR);
    read_at(&d, &held, day + DAY - 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), marked);
}

#[test]
fn a_read_the_next_day_moves_it() {
    let d = door();
    publish(&d);
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let held = d.minted(&fp);
    let day = T0 + 10 * DAY;
    read_at(&d, &held, day + DAY - 1);
    assert_eq!(record(&d, &fp)["last_read"], day);
    read_at(&d, &held, day + DAY);
    assert_eq!(record(&d, &fp)["last_read"], day + DAY);
}

#[test]
fn an_identity_read_on_a_day_lapses_idle_days_after_that_day() {
    let d = door();
    publish(&d);
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let held = d.minted(&fp);
    let day = T0 + 10 * DAY;
    read_at(&d, &held, day + 7 * HOUR);
    assert_eq!(
        scopes(&d.minted_at(&fp, None, day + 90 * DAY - 1).unwrap()).len(),
        2
    );
    assert_eq!(
        scopes(&d.minted_at(&fp, None, day + 90 * DAY).unwrap()),
        ["urn:cap:layer:say:join"]
    );
}

#[test]
fn an_identity_never_read_lapses_when_its_entitlement_says() {
    let d = door();
    let (_, fp) = certificate("alice");
    let joined_at = T0 + 15 * HOUR;
    let answer = join_at(&d, "alice", None, joined_at).unwrap();
    assert_eq!(answer["expires"], "2027-01-08T15:00:00.000Z");
    assert_eq!(
        scopes(&d.minted_at(&fp, None, joined_at + 90 * DAY - 1).unwrap()).len(),
        2
    );
    assert_eq!(
        scopes(&d.minted_at(&fp, None, joined_at + 90 * DAY).unwrap()),
        ["urn:cap:layer:say:join"]
    );
}

#[test]
fn a_read_on_the_day_of_the_join_does_not_shorten_it() {
    let d = door();
    publish(&d);
    let (_, fp) = certificate("alice");
    let joined_at = T0 + 15 * HOUR;
    join_at(&d, "alice", None, joined_at).unwrap();
    let held = d.minted_at(&fp, None, joined_at).expect("live");
    read_at(&d, &held, joined_at + HOUR);
    assert_eq!(
        scopes(&d.minted_at(&fp, None, joined_at + 90 * DAY - 1).unwrap()).len(),
        2
    );
}

#[test]
fn a_record_with_an_hour_in_it_loads_rounded_to_the_day() {
    let mut d = door();
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    // Opening reads the wall clock, so this record is dated from it.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let day = now / DAY * DAY - DAY;
    let mut old = record(&d, &fp);
    old["joined_at"] = (day - 3 * DAY + HOUR).into();
    old["last_read"] = (day + 5 * HOUR).into();
    let path = d.config.path().join(format!("joined/identities/{fp}.json"));
    std::fs::write(&path, old.to_string()).unwrap();

    d.restart();
    assert_eq!(
        record(&d, &fp)["last_read"],
        day,
        "rewritten at open, not at the next read, which may never come"
    );
    assert_eq!(record(&d, &fp)["joined_at"], day - 3 * DAY + HOUR);
    assert_eq!(scopes(&d.minted_at(&fp, None, now).unwrap()).len(), 2);
}

#[test]
fn join_idle_days_configures_the_limit() {
    let d = door_with("join_idle_days = 1\n");
    let (pem, fp) = certificate("alice");
    join_as(&d, &pem, &fp, &["releases"]).unwrap();
    let record: serde_json::Value =
        serde_json::from_str(&d.read(&format!("joined/identities/{fp}.json")).unwrap()).unwrap();
    let joined_at = record["joined_at"].as_u64().unwrap();
    assert_eq!(
        scopes(&d.minted_at(&fp, None, joined_at + DAY - 1).unwrap()).len(),
        2
    );
    assert_eq!(
        scopes(&d.minted_at(&fp, None, joined_at + DAY).unwrap()),
        ["urn:cap:layer:say:join"]
    );
}

#[test]
fn zero_is_refused_for_every_limit() {
    for key_name in [
        "join_idle_days",
        "per_certificate",
        "per_address",
        "per_hour",
        "max_joined",
    ] {
        let config = join_config(&format!("{key_name} = 0\n"));
        let state = Scratch::new("refused");
        let err = Host::open(config.path(), state.path(), &key(config.path()))
            .err()
            .map(|e| e.to_string())
            .unwrap();
        assert!(err.contains(&format!("`join.{key_name}` is 0")), "{err}");
    }
}

// 6. The descriptor.

fn run_descriptor(config: &Scratch, door_dir: &Scratch, out: &Scratch) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ikigai-layers"))
        .args(["descriptor"])
        .arg(config.path())
        .args(["--door", "quic://edge.example.org:4434", "--door-dir"])
        .arg(door_dir.path())
        .arg("--out")
        .arg(out.path())
        .output()
        .expect("the binary runs")
}

#[test]
fn the_descriptor_is_the_wire_contract() {
    let config = join_config("");
    let door_dir = Scratch::new("door-dir");
    let out = Scratch::new("out");
    let (pem, sha256) = certificate("server");
    door_dir.write("server.crt", &pem);

    let output = run_descriptor(&config, &door_dir, &out);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("wrote "));

    let json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(out.path().join("layers.json")).unwrap()).unwrap();
    assert_eq!(
        json,
        serde_json::json!({
            "door": "quic://edge.example.org:4434",
            "server_cert": { "url": "server.crt", "sha256": sha256 },
            "join": "urn:layer:join:say",
            "public_layers": [
                { "name": "features", "type": "ik:Feed", "read_only": true },
                { "name": "releases", "type": "ik:Feed", "read_only": true, "label": "Releases" },
            ],
        })
    );
    assert_eq!(
        std::fs::read_to_string(out.path().join("server.crt")).unwrap(),
        pem
    );
}

#[test]
fn the_descriptor_refuses_a_bad_address_and_a_host_with_nothing_to_offer() {
    let (pem, _) = certificate("server");
    let config = join_config("");
    let door_dir = Scratch::new("door-dir");
    door_dir.write("server.crt", &pem);
    let out = Scratch::new("out");

    let bad = Command::new(env!("CARGO_BIN_EXE_ikigai-layers"))
        .args(["descriptor"])
        .arg(config.path())
        .args(["--door", "https://edge.example.org", "--door-dir"])
        .arg(door_dir.path())
        .arg("--out")
        .arg(out.path())
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("not a door address"));

    config.remove("layers/join.toml");
    config.write("layers.toml", "");
    let none = run_descriptor(&config, &door_dir, &out);
    assert_eq!(none.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&none.stderr).contains("no layer says `join"));
}

// The operator's count (ledger #3309).

const COUNT: &str = "urn:layers:join:count";
const COUNT_CAP: &str = "urn:cap:layers:read:join-count";
const FEWER: &str = "fewer than 5";

/// The count's JSON face at `millis`, read as the operator.
fn count_at(d: &Door, millis: u64) -> serde_json::Value {
    let answer = source(
        &d.at(millis),
        COUNT,
        &[("as", "application/json")],
        &Capability::root(),
    );
    serde_json::from_str(&answer).expect("a JSON answer")
}

/// `how_many` certificates named `{group}{n}` joined at `T0`, each a capability to read with.
fn joined(d: &Door, group: &str, how_many: usize) -> Vec<Capability> {
    (0..how_many)
        .map(|n| {
            let name = format!("{group}{n}");
            let (_, fp) = certificate(&name);
            join(d, &name, &["releases"]).expect("joined");
            d.minted(&fp)
        })
        .collect()
}

fn count_of(
    joined: impl Into<serde_json::Value>,
    week: impl Into<serde_json::Value>,
    month: impl Into<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "joined": joined.into(),
        "readIn7Days": week.into(),
        "readIn30Days": month.into(),
    })
}

#[test]
fn the_count_answers_three_numbers_and_nothing_else() {
    let d = door();
    let readers = joined(&d, "reader", 6);
    assert_eq!(count_at(&d, T0), count_of(6, 6, 6));

    let plain = source(&d.at(T0), COUNT, &[], &Capability::root());
    assert_eq!(
        plain, "joined 6\nread-in-7-days 6\nread-in-30-days 6\n",
        "one labeled line per number"
    );
    for held in &readers {
        let fp = held.principal().unwrap().rsplit(':').next().unwrap();
        assert!(!plain.contains(fp), "{plain}");
    }
}

#[test]
fn a_count_under_five_is_reported_as_fewer_than_five() {
    let d = door();
    // Nobody at all reads the same as one reader: zero is under the threshold too.
    assert_eq!(count_at(&d, T0), count_of(FEWER, FEWER, FEWER));
    joined(&d, "one", 1);
    assert_eq!(count_at(&d, T0), count_of(FEWER, FEWER, FEWER));
    assert_eq!(
        source(&d.at(T0), COUNT, &[], &Capability::root()),
        "joined fewer than 5\nread-in-7-days fewer than 5\nread-in-30-days fewer than 5\n"
    );
    joined(&d, "more", 3);
    assert_eq!(count_at(&d, T0), count_of(FEWER, FEWER, FEWER));
    joined(&d, "last", 1);
    assert_eq!(
        count_at(&d, T0),
        count_of(5, 5, 5),
        "five is the first exact number"
    );
}

#[test]
fn each_window_counts_the_utc_days_ending_today() {
    let d = door();
    let this_week = joined(&d, "week", 5);
    let last_seen_seven_days_ago = joined(&d, "seven", 6);
    let this_month = joined(&d, "month", 5);
    // Never read after the join day, which is thirty days old by `today`.
    joined(&d, "thirty", 5);
    let today = T0 + 30 * DAY;
    for held in &this_week {
        read_at(&d, held, today - 6 * DAY + 23 * HOUR);
    }
    for held in &last_seen_seven_days_ago {
        read_at(&d, held, today - 7 * DAY + 23 * HOUR);
    }
    for held in &this_month {
        read_at(&d, held, today - 29 * DAY);
    }

    // The first and the last instant of the day read the same: a record holds only the day.
    for now in [today, today + DAY - 1] {
        assert_eq!(count_at(&d, now), count_of(21, 5, 16), "at {now}");
    }
    // The next midnight, those read six days ago are seven days old.
    assert_eq!(count_at(&d, today + DAY), count_of(21, FEWER, 11));
}

#[test]
fn a_join_counts_as_a_read_on_its_day() {
    let d = door();
    // A record cannot tell a join day with no read from a join day with one.
    joined(&d, "reader", 5);
    assert_eq!(count_at(&d, T0 + 6 * DAY), count_of(5, 5, 5));
    assert_eq!(count_at(&d, T0 + 7 * DAY), count_of(5, FEWER, 5));
    assert_eq!(count_at(&d, T0 + 30 * DAY), count_of(5, FEWER, FEWER));
}

#[test]
fn a_lapsed_identity_is_not_counted() {
    let d = door_with("join_idle_days = 10\n");
    let readers = joined(&d, "reader", 6);
    let stays = &readers[..5];
    for held in stays {
        read_at(&d, held, T0 + 9 * DAY);
    }
    assert_eq!(count_at(&d, T0 + 9 * DAY), count_of(6, 5, 6));
    // The sixth lapsed ten days after its join, and no request has told the host so yet.
    assert_eq!(count_at(&d, T0 + 10 * DAY + HOUR), count_of(5, 5, 5));
}

#[test]
fn reading_the_count_writes_nothing_and_a_restart_keeps_it() {
    let mut d = door();
    joined(&d, "reader", 5);
    let (_, fp) = certificate("reader0");
    let before = record(&d, &fp);
    count_at(&d, T0 + 40 * DAY);
    assert_eq!(
        record(&d, &fp),
        before,
        "the count reads the records and never moves one"
    );
    d.restart();
    assert_eq!(count_at(&d, T0), count_of(5, 5, 5));
}

#[test]
fn a_joined_identity_and_a_collaborators_grant_are_denied_the_count() {
    let d = door();
    joined(&d, "reader", 5);
    let (_, fp) = certificate("reader0");
    let asks = [vec![], vec![("as", "application/json")]];
    for args in &asks {
        let refused = issue(&d.kernel, Verb::Source, COUNT, args, &d.minted(&fp));
        assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    }

    let loaded = Config::load(d.config.path()).unwrap();
    let fingerprints = load_fingerprints(d.config.path(), &loaded).unwrap();
    let enrollment = generate(&loaded, &fingerprints).unwrap();
    assert!(!enrollment.grants.is_empty());
    for (slug, scopes) in &enrollment.grants {
        assert!(!scopes.iter().any(|s| s == COUNT_CAP), "{slug}: {scopes:?}");
        let scopes: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let refused = issue(
            &d.kernel,
            Verb::Source,
            COUNT,
            &[],
            &cap(&participant(slug), &scopes),
        );
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{slug}: {refused:?}"
        );
    }
}

#[test]
fn its_own_capability_reads_the_count() {
    let d = door();
    joined(&d, "reader", 5);
    assert_eq!(
        source(&d.kernel, COUNT, &[], &Capability::scoped([COUNT_CAP])),
        "joined 5\nread-in-7-days 5\nread-in-30-days 5\n"
    );
}

#[test]
fn a_host_with_no_join_has_no_count() {
    let f = fixture();
    let missing = issue(&f.kernel, Verb::Source, COUNT, &[], &Capability::root());
    assert!(matches!(missing, Err(Error::Unresolved(_))), "{missing:?}");
}

#[test]
fn an_unknown_face_of_the_count_is_refused() {
    let d = door();
    let refused = issue(
        &d.kernel,
        Verb::Source,
        COUNT,
        &[("as", "text/turtle")],
        &Capability::root(),
    );
    match refused {
        Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, "as"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_counts_description_says_what_the_numbers_are_not() {
    let d = door();
    let meta = issue(
        &d.kernel,
        Verb::Meta,
        COUNT,
        &[("as", "application/json")],
        &Capability::root(),
    )
    .expect("a description");
    let json: serde_json::Value = serde_json::from_slice(&meta.bytes).unwrap();
    let described = json.to_string();
    for phrase in [
        "upper bounds on people",
        "several certificates",
        "enrolled identities are not counted",
        "reads are not recorded",
    ] {
        assert!(described.contains(phrase), "{phrase}: {described}");
    }
    assert!(!described.contains("  "), "{described}");
}

/// A join request supersedes only its own author's entry, as a `say` does: the join path
/// stores what the lobby admits, and an operator's join shapes may admit `ik:supersedes`, so
/// without the host's own check one stranger's request retires another's in `latest`.
#[test]
fn a_join_request_never_supersedes_another_joiners() {
    let config = join_config("");
    let shapes = include_str!("../onboard/layers/join.shapes.ttl").replace(
        "    [ sh:path prov:wasAttributedTo ;",
        "    [ sh:path ik:supersedes ; sh:nodeKind sh:IRI ] ,\n    [ sh:path prov:wasAttributedTo ;",
    );
    assert!(shapes.contains("ik:supersedes"), "{shapes}");
    config.write("layers/join.shapes.ttl", &shapes);
    let state = Scratch::new("join-state");
    let host = Host::open(config.path(), state.path(), &key(config.path())).expect("the host");
    let kernel = kernel_over(&host);
    let d = Door {
        config,
        state,
        host,
        kernel,
    };

    join(&d, "alice", &["releases"]).expect("alice joins");
    let reader = cap(&gonk(), &["urn:cap:layer:read:join"]);
    let entries = source(&d.kernel, "urn:layer:join:entries", &[], &reader);
    let alices = triples(&entries)
        .iter()
        .map(|t| t.subject.to_string())
        .find(|s| s.starts_with("<urn:layer:join:entry:"))
        .expect("alice's request")
        .trim_matches(|c| c == '<' || c == '>')
        .to_string();

    let (pem, fp) = certificate("mallory");
    let content = request_turtle(&pem, &fp, &["releases"]).replace(
        "onb:JoinRequest ;",
        &format!("onb:JoinRequest ;\n  <https://ikigai-rs.dev/ns#supersedes> <{alices}> ;"),
    );
    let refused = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &content), ("as", "application/json")],
        &d.minted(&fp),
    );
    match refused {
        Err(Error::Denied(m)) => assert!(m.contains("another participant's entry"), "{m}"),
        other => panic!("mallory's request superseded alice's: {other:?}"),
    }

    // And one that supersedes no entry at all is refused as a `say` refuses it.
    let (pem, fp) = certificate("trent");
    let content = request_turtle(&pem, &fp, &["releases"]).replace(
        "onb:JoinRequest ;",
        "onb:JoinRequest ;\n  <https://ikigai-rs.dev/ns#supersedes> <urn:layer:join:entry:nothing> ;",
    );
    let refused = issue(
        &d.kernel,
        Verb::Sink,
        SAY,
        &[("content", &content), ("as", "application/json")],
        &d.minted(&fp),
    );
    assert!(
        matches!(refused, Err(Error::InvalidArgument { .. })),
        "{refused:?}"
    );
}
