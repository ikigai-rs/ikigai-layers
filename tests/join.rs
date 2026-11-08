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
use ikigai_layers::onboard::{fingerprint_of_pem, signing_bytes_of};
use ikigai_layers::Host;

const DOOR: &str = "edge";
const SAY: &str = "urn:layer:join:say";
const RELEASES_ENTRIES: &str = "urn:layer:releases:entries";
/// The instant [`TickingClock`] starts from.
const T0: u64 = 1_791_590_400_000;
const DAY: u64 = 24 * 60 * 60 * 1000;
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
fn the_per_address_limit_survives_a_restart() {
    let mut d = door_with("per_address = 2\n");
    let addr = Some("198.51.100.7:4433");
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

    d.restart();
    d.minted_at(&fp, addr, T0).expect("a lobby");
    unavailable(
        join_as(&d, &pem, &fp, &["releases"]),
        "from one source address",
    );
    // Another address is another count: the door told the host which.
    let (pem, fp) = certificate("dana");
    d.minted_at(&fp, Some("198.51.100.8:4433"), T0)
        .expect("a lobby");
    join_as(&d, &pem, &fp, &["releases"]).expect("another address joins");
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
