//! Onboarding a node by itself (ledger #2009): the lobby, the enrollment request's checks,
//! and applying signed entitlements to the door's enrollment.
//!
//! The layer pair is the repository's own example (`onboard/`), read here as it ships, so the
//! example cannot drift from what the tests hold. Every key is generated in the test from a
//! seed the test picks, and every certificate is a well-formed DER document built here.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine as _;
use common::*;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::spki::EncodePublicKey;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use futures::executor::block_on;
use ikigai_core::{
    ArgRef, Capability, Description, Endpoint, EndpointSpace, Error, Exact, Fallback, Invocation,
    Iri, Kernel, ReprType, Representation, Request, Space, Verb,
};
use ikigai_layers::onboard::{
    fingerprint_of_pem, lobby_capability, signing_bytes_of, Verdict, LOBBY_SAYS_PER_FINGERPRINT,
    LOBBY_SAYS_TOTAL, STATUS,
};
use ikigai_layers::{Config, Host};

const EXAMPLE: &[(&str, &str)] = &[
    (
        "layers/enroll.toml",
        include_str!("../onboard/layers/enroll.toml"),
    ),
    (
        "layers/enroll.shapes.ttl",
        include_str!("../onboard/layers/enroll.shapes.ttl"),
    ),
    (
        "layers/enroll.context.jsonld",
        include_str!("../onboard/layers/enroll.context.jsonld"),
    ),
    (
        "layers/grants.toml",
        include_str!("../onboard/layers/grants.toml"),
    ),
    (
        "layers/grants.shapes.ttl",
        include_str!("../onboard/layers/grants.shapes.ttl"),
    ),
    (
        "layers/grants.context.jsonld",
        include_str!("../onboard/layers/grants.context.jsonld"),
    ),
    (
        "collabs/onboard.toml",
        include_str!("../onboard/collabs/onboard.toml"),
    ),
];

/// The edge collaboration: a `tasks` tuplespace an entitlement can grant, mounted by Brian.
const EDGE_TOML: &str = r#"[[mount]]
layer = "tasks"
brian = ["read", "feed", "out", "take"]
"#;

const CONTROLLER_SEED: [u8; 32] = [0x11; 32];
const STRANGER_SEED: [u8; 32] = [0x22; 32];

fn public_pem(seed: [u8; 32]) -> String {
    SigningKey::from_bytes(&seed)
        .verifying_key()
        .to_public_key_pem(LineEnding::LF)
        .expect("an SPKI PEM")
}

fn private_pem(seed: [u8; 32]) -> String {
    SigningKey::from_bytes(&seed)
        .to_pkcs8_pem(LineEnding::LF)
        .expect("a PKCS8 PEM")
        .to_string()
}

/// A config directory: the common fixture, the example onboarding pair, a `tasks` layer, and
/// an `[onboard]` table trusting the controller's key.
fn onboard_config(trust_reload: &str) -> Scratch {
    let config = Scratch::new("onboard");
    write_config(&config);
    for (path, text) in EXAMPLE {
        config.write(path, text);
    }
    config.write(
        "layers/tasks.toml",
        "model = \"tuplespace\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\n",
    );
    config.write("collabs/edge.toml", EDGE_TOML);
    config.write("controller.pub.pem", &public_pem(CONTROLLER_SEED));
    config.write(
        "layers.toml",
        &format!(
            "[onboard]\ngrants = \"grants\"\ncontroller = \"brian\"\ncontroller_key = \"controller.pub.pem\"\n\
             trust_dir = \"trust\"\ntrust_reload = \"{trust_reload}\"\n"
        ),
    );
    config
}

struct Edge {
    config: Scratch,
    state: Scratch,
    host: Arc<Host>,
    kernel: Kernel,
}

fn edge(trust_reload: &str) -> Edge {
    let config = onboard_config(trust_reload);
    let state = Scratch::new("onboard-state");
    let host = Host::open(config.path(), state.path(), &key(config.path())).expect("the host");
    let kernel = kernel_over(&host);
    Edge {
        config,
        state,
        host,
        kernel,
    }
}

impl Edge {
    /// Open the host again over the same directories, the old one dropped first (it holds the
    /// state directory's lock).
    fn reopen(&mut self) {
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

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.config.path().join(rel)).ok()
    }
}

/// A certificate PEM: one well-formed DER document (a SEQUENCE holding an OCTET STRING), so
/// the host's PEM and DER checks pass and its fingerprint is the SHA-256 of these bytes.
fn certificate(name: &str) -> String {
    let body = format!("a test certificate for {name}").into_bytes();
    let mut der = vec![0x30, (body.len() + 2) as u8, 0x04, body.len() as u8];
    der.extend(body);
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    format!("-----BEGIN CERTIFICATE-----\n{b64}\n-----END CERTIFICATE-----\n")
}

fn literal(value: &str) -> String {
    oxrdf::Literal::new_simple_literal(value).to_string()
}

const ONB: &str = "@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .\n@prefix ik: <https://ikigai-rs.dev/ns#> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n";

/// A request as the node writes it: the contract's subject, `<urn:iki:onboard:request:{fp}>`.
fn request_turtle(pem: &str, fingerprint: &str) -> String {
    request_about(
        &format!("urn:iki:onboard:request:{fingerprint}"),
        pem,
        fingerprint,
    )
}

fn request_about(subject: &str, pem: &str, fingerprint: &str) -> String {
    format!(
        "{ONB}<{subject}> a onb:EnrollmentRequest ;\n  onb:invite <urn:iki:onboard:invite:mini-1> ;\n  \
         onb:certificate {} ;\n  onb:fingerprint {} ;\n  onb:proof {} .\n",
        literal(pem),
        literal(fingerprint),
        literal(&"0".repeat(64)),
    )
}

/// Say a request on the lobby as the certificate `pem`, through the capability a door mints
/// for a certificate it did not enroll.
fn request(edge: &Edge, pem: &str) -> Result<String, Error> {
    let fp = fingerprint_of_pem(pem).unwrap();
    let cap = lobby_capability("enroll", "edge", &fp).unwrap();
    issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:enroll:say",
        &[("content", &request_turtle(pem, &fp))],
        &cap,
    )
    .map(|r| text(&r))
}

/// The controller's key, served to ikigai-sign's `urn:sign:sign` in a kernel of the test's own.
struct PrivateKey(Vec<u8>);

#[async_trait]
impl Endpoint for PrivateKey {
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

/// `body` (an entitlement about `<urn:layer:new>`) signed with the key of `seed`, as the
/// controller's tools would: ikigai-sign over the host's signing bytes.
fn signed(body: &str, seed: [u8; 32]) -> String {
    let bytes = signing_bytes_of(body, "urn:layer:new").unwrap();
    let keys = EndpointSpace::new().bind(
        Exact::new("urn:test:key"),
        PrivateKey(private_pem(seed).into_bytes()),
    );
    let spaces: Vec<Arc<dyn Space>> = vec![Arc::new(ikigai_sign::space()), Arc::new(keys)];
    let signer = Kernel::new(Arc::new(Fallback::new(spaces)));
    let request = Request::new(Verb::Source, Iri::parse("urn:sign:sign").unwrap())
        .with_arg("in", ArgRef::Inline(bytes))
        .with_arg("key", ArgRef::Inline(b"urn:test:key".to_vec()));
    let graph =
        block_on(signer.issue(request, &Capability::scoped(["urn:cap:sign"]))).expect("signed");
    let sig = String::from_utf8(graph.bytes).unwrap();
    let body = body.trim_end().trim_end_matches('.').trim_end();
    format!("{body} ;\n  onb:signature {} .\n", literal(&sig))
}

struct Grant<'a> {
    fingerprint: &'a str,
    name: &'a str,
    request: Option<&'a str>,
    grants: &'a [&'a str],
    supersedes: Option<&'a str>,
    expires: Option<&'a str>,
}

fn entitlement_body(g: &Grant<'_>) -> String {
    let mut lines = vec![
        format!("onb:fingerprint {}", literal(g.fingerprint)),
        format!("onb:principal <urn:collab:participant:{}>", g.name),
        "onb:kind <urn:iki:onboard:kind:task-member>".to_string(),
        "onb:gonkGrant \"default=worker\"".to_string(),
    ];
    if let Some(r) = g.request {
        lines.push(format!("onb:request <{r}>"));
    }
    for cap in g.grants {
        lines.push(format!("onb:layerGrant <{cap}>"));
    }
    if let Some(s) = g.supersedes {
        lines.push(format!("ik:supersedes <{s}>"));
    }
    if let Some(e) = g.expires {
        lines.push(format!("onb:expires \"{e}\"^^xsd:dateTime"));
    }
    format!(
        "{ONB}<urn:layer:new> a onb:Entitlement ;\n  {} .\n",
        lines.join(" ;\n  ")
    )
}

fn controller() -> Capability {
    cap(
        &participant("brian"),
        &[
            "urn:cap:layer:say:grants",
            "urn:cap:layer:read:grants",
            "urn:cap:layer:read:enroll",
        ],
    )
}

/// Say an entitlement on the grants layer as the controller.
fn entitle(edge: &Edge, content: &str) -> String {
    text(
        &issue(
            &edge.kernel,
            Verb::Sink,
            "urn:layer:grants:say",
            &[("content", content)],
            &controller(),
        )
        .expect("the grants layer admits the entitlement"),
    )
    .trim()
    .to_string()
}

fn status(edge: &Edge) -> serde_json::Value {
    serde_json::from_str(&source(
        &edge.kernel,
        STATUS,
        &[("as", "application/json")],
        &controller(),
    ))
    .unwrap()
}

/// The capability the door would mint for `fingerprint` from the files the host wrote: its
/// `clients.json` entry's grant and principal, and that grant's scopes in `grants.json`.
fn door_mint(edge: &Edge, fingerprint: &str) -> Option<Capability> {
    let clients: serde_json::Value = serde_json::from_str(&edge.read("clients.json")?).unwrap();
    let grants: BTreeMap<String, Vec<String>> =
        serde_json::from_str(&edge.read("grants.json")?).unwrap();
    let client = clients["clients"].get(fingerprint)?;
    let scopes = &grants[client["grant"].as_str().unwrap()];
    Some(cap(
        client["principal"].as_str().unwrap(),
        &scopes.iter().map(String::as_str).collect::<Vec<_>>(),
    ))
}

const TASKS: &[&str] = &[
    "urn:cap:layer:read:tasks",
    "urn:cap:layer:out:tasks",
    "urn:cap:layer:take:tasks",
];

/// The actions a capability may invoke, by the predicate `urn:kernel:actions` uses.
fn manifold(kernel: &Kernel, capability: &Capability) -> BTreeSet<String> {
    let mut offered = BTreeSet::new();
    for entry in kernel.entries().expect("an enumerable kernel") {
        let Some(description) = kernel.describe_pattern(&entry.pattern) else {
            continue;
        };
        for action in description.action_specs() {
            if description
                .unsatisfied_scopes(action.verb, capability)
                .is_empty()
            {
                offered.insert(format!("{}__{:?}", description.id, action.verb).to_lowercase());
            }
        }
    }
    offered
}

// ---- the request ------------------------------------------------------------------------

#[test]
fn a_request_is_admitted_from_the_certificate_it_names() {
    let edge = edge("live");
    let pem = certificate("mini");
    let iri = request(&edge, &pem).expect("admitted");
    assert!(iri.starts_with("urn:layer:enroll:entry:"), "{iri}");
}

#[test]
fn a_request_for_another_certificate_is_refused() {
    let edge = edge("live");
    let mine = certificate("mini");
    let theirs = certificate("someone else");
    let fp_theirs = fingerprint_of_pem(&theirs).unwrap();
    // Posted by `mine`, stating (and carrying) someone else's certificate.
    let cap = lobby_capability("enroll", "edge", &fingerprint_of_pem(&mine).unwrap()).unwrap();
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:enroll:say",
        &[("content", &request_turtle(&theirs, &fp_theirs))],
        &cap,
    )
    .unwrap_err();
    assert!(
        matches!(&refused, Error::Denied(m) if m.contains("not the certificate that said it")),
        "{refused}"
    );
}

#[test]
fn a_certificate_that_does_not_hash_to_its_fingerprint_is_refused() {
    let edge = edge("live");
    let mine = certificate("mini");
    let fp = fingerprint_of_pem(&mine).unwrap();
    let other = certificate("another");
    let cap = lobby_capability("enroll", "edge", &fp).unwrap();
    // The fingerprint is the poster's, the certificate is not.
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:enroll:say",
        &[("content", &request_turtle(&other, &fp))],
        &cap,
    )
    .unwrap_err();
    assert!(
        matches!(&refused, Error::InvalidArgument { detail, .. }
            if detail.contains("hashes to") && detail.contains(&fp)),
        "{refused}"
    );
    // And a body that is no certificate at all.
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:enroll:say",
        &[(
            "content",
            &request_turtle(
                "-----BEGIN CERTIFICATE-----\nnot base64\n-----END CERTIFICATE-----\n",
                &fp,
            ),
        )],
        &cap,
    )
    .unwrap_err();
    assert!(
        matches!(refused, Error::InvalidArgument { .. }),
        "{refused}"
    );
}

#[test]
fn an_enrolled_participant_cannot_request_for_a_certificate() {
    // Its name carries no fingerprint, so the host cannot tie the certificate to it.
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:enroll:say",
        &[("content", &request_turtle(&pem, &fp))],
        &cap(&participant("brian"), &["urn:cap:layer:say:enroll"]),
    )
    .unwrap_err();
    assert!(
        matches!(&refused, Error::Denied(m) if m.contains("carries none")),
        "{refused}"
    );
}

/// A call: its verb, its IRI and its arguments.
type Call<'a> = (Verb, &'a str, &'a [(&'a str, &'a str)]);

#[test]
fn a_lobby_takes_the_placeholder_subject_too_but_not_another_fingerprints_name() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let lobby = lobby_capability("enroll", "edge", &fp).unwrap();
    let say = |content: &str| {
        issue(
            &edge.kernel,
            Verb::Sink,
            "urn:layer:enroll:say",
            &[("content", content)],
            &lobby,
        )
    };
    // The host names the entry whichever subject the node wrote.
    let iri = text(&say(&request_about("urn:layer:new", &pem, &fp)).unwrap());
    assert!(iri.starts_with("urn:layer:enroll:entry:"), "{iri}");
    let stored = source(
        &edge.kernel,
        iri.trim(),
        &[],
        &cap(&participant("brian"), &["urn:cap:layer:read:enroll"]),
    );
    assert!(!stored.contains("urn:iki:onboard:request:"), "{stored}");

    // A request named for another fingerprint than the one it states is refused.
    let other = "cd".repeat(32);
    let refused = say(&request_about(
        &format!("urn:iki:onboard:request:{other}"),
        &pem,
        &fp,
    ))
    .unwrap_err();
    assert!(
        matches!(&refused, Error::InvalidArgument { detail, .. } if detail.contains("is named for")),
        "{refused}"
    );
    // And the request name is a lobby's alone: no other layer takes it.
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:talk:say",
        &[("content", &request_turtle(&pem, &fp))],
        &member(&participant("brian")),
    )
    .unwrap_err();
    assert!(
        matches!(refused, Error::InvalidArgument { .. }),
        "{refused}"
    );
}

// ---- the lobby ----------------------------------------------------------------------------

#[test]
fn an_unenrolled_certificate_can_say_on_the_lobby_and_nothing_else() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let lobby = edge.host.lobby_capability("edge", &fp).unwrap().unwrap();
    assert_eq!(lobby, lobby_capability("enroll", "edge", &fp).unwrap());

    // Its whole manifold of this host's resources is the lobby's say (the kernel's own open
    // `urn:kernel:actions` and `urn:kernel:validate` are every caller's).
    assert_eq!(
        manifold(&edge.kernel, &lobby)
            .into_iter()
            .filter(|a| !a.starts_with("kernel-"))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["layer-enroll-say__sink".to_string()])
    );
    request(&edge, &pem).expect("the lobby's say");

    // Every other verb on the lobby, and every other layer and host-wide read, is Denied.
    let denied: &[Call] = &[
        (Verb::Source, "urn:layer:enroll", &[]),
        (Verb::Source, "urn:layer:enroll:entries", &[]),
        (Verb::Source, "urn:layer:enroll:feed", &[]),
        (Verb::Source, "urn:layer:enroll:latest", &[]),
        (Verb::Source, "urn:layer:grants:entries", &[]),
        (Verb::Sink, "urn:layer:grants:say", &[("content", "x")]),
        (Verb::Sink, "urn:layer:talk:say", &[("content", "x")]),
        (Verb::Source, "urn:layer:tasks:tuples", &[]),
        (Verb::Sink, "urn:layer:tasks:tuples", &[("content", "x")]),
        (
            Verb::Delete,
            "urn:layer:tasks:tuples",
            &[("subject", "urn:x")],
        ),
        (Verb::Source, "urn:layers:feed", &[]),
        (Verb::Source, "urn:layers:participants", &[]),
        (Verb::Source, "urn:layers:me", &[]),
        (Verb::Source, "urn:collab:onboard", &[]),
        (Verb::Source, STATUS, &[]),
    ];
    for (verb, iri, args) in denied {
        let refused = issue(&edge.kernel, *verb, iri, args, &lobby).unwrap_err();
        assert!(
            matches!(refused, Error::Denied(_)),
            "{verb:?} {iri}: {refused}"
        );
    }

    // A host with no lobby gives a door nothing to mint.
    let f = fixture();
    assert_eq!(f.host.lobby_capability("edge", &fp).unwrap(), None);
}

#[test]
fn a_lobby_takes_five_says_an_hour_from_one_certificate() {
    let edge = edge("live");
    let pem = certificate("mini");
    for _ in 0..LOBBY_SAYS_PER_FINGERPRINT {
        request(&edge, &pem).expect("within the bound");
    }
    let refused = request(&edge, &pem).unwrap_err();
    assert!(
        matches!(&refused, Error::Unavailable(m) if m.contains("at most 5 says an hour from one certificate")),
        "{refused}"
    );
    // Another certificate is not held to this one's count.
    request(&edge, &certificate("another")).expect("another certificate");
}

#[test]
fn a_lobby_takes_thirty_says_an_hour_from_everyone() {
    let edge = edge("live");
    let mut said = 0;
    'outer: for n in 0.. {
        let pem = certificate(&format!("node {n}"));
        for _ in 0..LOBBY_SAYS_PER_FINGERPRINT {
            if said == LOBBY_SAYS_TOTAL {
                break 'outer;
            }
            request(&edge, &pem).expect("within both bounds");
            said += 1;
        }
    }
    let refused = request(&edge, &certificate("one more")).unwrap_err();
    assert!(
        matches!(&refused, Error::Unavailable(m) if m.contains("30 says this hour from everyone")),
        "{refused}"
    );
}

#[test]
fn a_lobby_refuses_an_entry_over_sixteen_kib() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let cap = lobby_capability("enroll", "edge", &fp).unwrap();
    let big = format!("{}# {}\n", request_turtle(&pem, &fp), "x".repeat(16 * 1024));
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:enroll:say",
        &[("content", &big)],
        &cap,
    )
    .unwrap_err();
    assert!(
        matches!(&refused, Error::InvalidArgument { detail, .. } if detail.contains("bound of 16384")),
        "{refused}"
    );
}

// ---- entitlements -------------------------------------------------------------------------

#[test]
fn an_entitlement_signed_by_the_controller_is_applied_and_the_node_reads_tasks() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();

    // Before: the door would refuse the certificate.
    assert!(door_mint(&edge, &fp).is_none());

    let e1 = entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                fingerprint: &fp,
                name: "mini",
                request: Some(&req),
                grants: TASKS,
                supersedes: None,
                expires: Some("2099-01-01T00:00:00Z"),
            }),
            CONTROLLER_SEED,
        ),
    );

    let s = status(&edge);
    assert_eq!(s["seen"], 1, "{s}");
    assert_eq!(s["applied"], 1, "{s}");
    assert_eq!(s["records"][0]["verdict"], "applied", "{s}");
    assert_eq!(s["effective"][0]["fingerprint"], fp, "{s}");
    assert_eq!(s["restart_pending"], false, "{s}");

    // The files the door reads: the participant, the grant, the client, the certificate.
    assert!(edge
        .read("participants.ttl")
        .unwrap()
        .contains("<urn:collab:participant:mini> a <https://ikigai-rs.dev/ns#Machine>"));
    assert!(edge.read("entitlements.toml").unwrap().contains(&e1));
    assert_eq!(edge.read(&format!("trust/{fp}.crt")).unwrap(), pem);
    let minted = door_mint(&edge, &fp).expect("enrolled");
    assert_eq!(minted.principal(), Some(participant("mini").as_str()));

    // The generator, run by hand over the same directory, derives exactly the same files.
    let config = Config::load(edge.config.path()).unwrap();
    let fingerprints =
        ikigai_layers::grants::load_fingerprints(edge.config.path(), &config).unwrap();
    let enrollment = ikigai_layers::grants::generate(&config, &fingerprints).unwrap();
    assert_eq!(edge.read("grants.json").unwrap(), enrollment.grants_json());
    assert_eq!(
        edge.read("clients.json").unwrap(),
        enrollment.clients_json()
    );

    // The node, minted as the door would mint it, reads (and writes) tasks, and nothing of
    // talk. The host re-reads participants per call, so no restart is needed in-process.
    source(&edge.kernel, "urn:layer:tasks:tuples", &[], &minted);
    issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:tasks:tuples",
        &[(
            "content",
            &offer("from-mini").replace("tasks-bobby", "tasks"),
        )],
        &minted,
    )
    .expect("the node drops a tuple");
    let refused = issue(
        &edge.kernel,
        Verb::Source,
        "urn:layer:talk:entries",
        &[],
        &minted,
    )
    .unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused}");
}

#[test]
fn an_entitlement_signed_by_anyone_else_is_not_applied() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    let body = entitlement_body(&Grant {
        fingerprint: &fp,
        name: "mini",
        request: Some(&req),
        grants: TASKS,
        supersedes: None,
        expires: None,
    });
    entitle(&edge, &signed(&body, STRANGER_SEED));
    // And one signed by the controller, then altered after signing.
    let altered = signed(&body, CONTROLLER_SEED)
        .replace("<urn:cap:layer:read:tasks>", "<urn:cap:layer:read:talk>");
    entitle(&edge, &altered);

    let s = status(&edge);
    assert_eq!(s["seen"], 2, "{s}");
    assert_eq!(s["applied"], 0, "{s}");
    assert_eq!(s["refused"], 2, "{s}");
    for record in s["records"].as_array().unwrap() {
        assert_eq!(record["verdict"], "refused", "{s}");
        assert!(
            record["reason"]
                .as_str()
                .unwrap()
                .contains("does not verify"),
            "{s}"
        );
    }
    assert!(door_mint(&edge, &fp).is_none());
    assert!(edge.read(&format!("trust/{fp}.crt")).is_none());
    assert!(edge
        .read("entitlements.toml")
        .is_none_or(|t| !t.contains("[[entitled]]")));
}

#[test]
fn applying_is_idempotent_and_a_replay_changes_nothing() {
    let mut edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    let content = signed(
        &entitlement_body(&Grant {
            fingerprint: &fp,
            name: "mini",
            request: Some(&req),
            grants: TASKS,
            supersedes: None,
            expires: None,
        }),
        CONTROLLER_SEED,
    );
    entitle(&edge, &content);
    let files = |edge: &Edge| {
        [
            "participants.ttl",
            "entitlements.toml",
            "grants.json",
            "clients.json",
        ]
        .map(|f| edge.read(f))
    };
    let before = files(&edge);

    // Said again, the same signed bytes are a replay: refused, and nothing moves.
    entitle(&edge, &content);
    assert_eq!(files(&edge), before);
    let s = status(&edge);
    assert_eq!(s["records"][1]["verdict"], "refused", "{s}");
    assert!(
        s["records"][1]["reason"]
            .as_str()
            .unwrap()
            .contains("already entitled"),
        "{s}"
    );

    // A restart applies the same layer to the same files.
    edge.reopen();
    assert_eq!(files(&edge), before);
    let again = status(&edge);
    assert_eq!(again["effective"], s["effective"]);
    assert_eq!(again["records"], s["records"]);
    // The participant was appended once.
    assert_eq!(
        edge.read("participants.ttl")
            .unwrap()
            .matches("<urn:collab:participant:mini>")
            .count(),
        1
    );
}

#[test]
fn a_revocation_removes_the_grants_and_the_certificate() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    let grant = Grant {
        fingerprint: &fp,
        name: "mini",
        request: Some(&req),
        grants: TASKS,
        supersedes: None,
        expires: None,
    };
    let e1 = entitle(&edge, &signed(&entitlement_body(&grant), CONTROLLER_SEED));
    assert!(door_mint(&edge, &fp).is_some());

    // A revocation that supersedes nothing is refused...
    entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                grants: &[],
                request: None,
                ..grant
            }),
            CONTROLLER_SEED,
        ),
    );
    assert!(door_mint(&edge, &fp).is_some());

    // ...and one that supersedes the current entitlement removes everything it granted.
    let e3 = entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                grants: &[],
                request: None,
                supersedes: Some(&e1),
                ..grant
            }),
            CONTROLLER_SEED,
        ),
    );
    assert!(door_mint(&edge, &fp).is_none());
    assert!(edge.read(&format!("trust/{fp}.crt")).is_none());
    let grants: BTreeMap<String, Vec<String>> =
        serde_json::from_str(&edge.read("grants.json").unwrap()).unwrap();
    assert!(!grants.contains_key("mini"), "{grants:?}");
    let s = status(&edge);
    assert_eq!(s["effective"], serde_json::json!([]), "{s}");
    assert_eq!(s["records"][0]["verdict"], "superseded", "{s}");
    assert_eq!(s["records"][0]["by"], e3, "{s}");
    assert_eq!(s["records"][1]["verdict"], "refused", "{s}");
    assert_eq!(s["records"][2]["verdict"], "revoked", "{s}");

    // Re-entitled, superseding the revocation: back in force.
    entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                supersedes: Some(&e3),
                ..grant
            }),
            CONTROLLER_SEED,
        ),
    );
    assert!(door_mint(&edge, &fp).is_some());
    assert_eq!(edge.read(&format!("trust/{fp}.crt")).unwrap(), pem);
}

#[test]
fn an_expired_entitlement_grants_nothing() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                fingerprint: &fp,
                name: "mini",
                request: Some(&req),
                grants: TASKS,
                supersedes: None,
                expires: Some("2020-01-01T00:00:00Z"),
            }),
            CONTROLLER_SEED,
        ),
    );
    assert_eq!(status(&edge)["records"][0]["verdict"], "expired");
    assert!(door_mint(&edge, &fp).is_none());
}

#[test]
fn an_entitlement_naming_another_certificates_request_is_refused() {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let theirs = request(&edge, &certificate("someone else")).unwrap();
    entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                fingerprint: &fp,
                name: "mini",
                request: Some(&theirs),
                grants: TASKS,
                supersedes: None,
                expires: None,
            }),
            CONTROLLER_SEED,
        ),
    );
    let s = status(&edge);
    assert!(
        s["records"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("another certificate"),
        "{s}"
    );
    assert!(door_mint(&edge, &fp).is_none());
}

#[test]
fn a_door_that_reads_its_trust_list_at_start_is_told_to_restart() {
    let edge = edge("start");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    assert_eq!(status(&edge)["restart_pending"], false);
    entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                fingerprint: &fp,
                name: "mini",
                request: Some(&req),
                grants: TASKS,
                supersedes: None,
                expires: None,
            }),
            CONTROLLER_SEED,
        ),
    );
    assert_eq!(status(&edge)["restart_pending"], true);
    assert!(source(&edge.kernel, STATUS, &[], &controller()).contains("restart the door"));
    // The status is the controller's: the node, entitled to tasks only, cannot read it.
    let node = door_mint(&edge, &fp).unwrap();
    assert!(matches!(
        issue(&edge.kernel, Verb::Source, STATUS, &[], &node).unwrap_err(),
        Error::Denied(_)
    ));
}

#[test]
fn the_record_says_what_each_verdict_is() {
    // The serialized verdicts are a contract the controller's tools read.
    let applied = serde_json::to_value(Verdict::Applied).unwrap();
    assert_eq!(applied, serde_json::json!({"verdict": "applied"}));
    let refused = serde_json::to_value(Verdict::Refused {
        reason: "why".into(),
    })
    .unwrap();
    assert_eq!(
        refused,
        serde_json::json!({"verdict": "refused", "reason": "why"})
    );
}

#[test]
fn the_node_reads_its_entitlement_with_its_new_certificate() {
    // The node polls `urn:layer:grants:entries type=<onb:Entitlement>` as the certificate it
    // just enrolled, so an applied entitlement carries `read` on the grants layer.
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    let e1 = entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                fingerprint: &fp,
                name: "mini",
                request: Some(&req),
                grants: TASKS,
                supersedes: None,
                expires: None,
            }),
            CONTROLLER_SEED,
        ),
    );
    let node = door_mint(&edge, &fp).unwrap();
    let graph = source(
        &edge.kernel,
        "urn:layer:grants:entries",
        &[
            ("type", "https://ikigai-rs.dev/ns/onboard#Entitlement"),
            ("as", "application/ld+json"),
        ],
        &node,
    );
    assert!(graph.contains(&e1) && graph.contains(&fp), "{graph}");
    // It reads; it never says.
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:grants:say",
        &[("content", "x")],
        &node,
    )
    .unwrap_err();
    assert!(matches!(refused, Error::Denied(_)), "{refused}");
}

#[test]
fn only_the_controller_says_on_the_grants_layer() {
    // At runtime: a principal holding the token, who is not the controller, is refused.
    let edge = edge("live");
    let refused = issue(
        &edge.kernel,
        Verb::Sink,
        "urn:layer:grants:say",
        &[("content", "x")],
        &cap(&participant("chris"), &["urn:cap:layer:say:grants"]),
    )
    .unwrap_err();
    assert!(
        matches!(&refused, Error::Denied(m) if m.contains("only the onboarding controller")),
        "{refused}"
    );

    // At open: a manifest granting `say` on it to anyone else stops the host.
    let config = onboard_config("live");
    config.write(
        "collabs/onboard.toml",
        "[[mount]]\nlayer = \"grants\"\nbrian = [\"read\", \"say\"]\nchris = [\"say\"]\n",
    );
    let state = Scratch::new("onboard-state");
    let e = Host::open(config.path(), state.path(), &key(config.path()))
        .err()
        .expect("refused")
        .to_string();
    assert!(e.contains("only onboard.controller"), "{e}");

    // And an entitlement granting more than read on it is refused.
    let edge = edge_with_entitlement(&["urn:cap:layer:say:grants"]);
    let s = status(&edge);
    assert!(
        s["records"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("only the controller says"),
        "{s}"
    );
}

fn edge_with_entitlement(grants: &[&str]) -> Edge {
    let edge = edge("live");
    let pem = certificate("mini");
    let fp = fingerprint_of_pem(&pem).unwrap();
    let req = request(&edge, &pem).unwrap();
    entitle(
        &edge,
        &signed(
            &entitlement_body(&Grant {
                fingerprint: &fp,
                name: "mini",
                request: Some(&req),
                grants,
                supersedes: None,
                expires: None,
            }),
            CONTROLLER_SEED,
        ),
    );
    edge
}

// ---- configuration ------------------------------------------------------------------------

#[test]
fn a_lobby_is_a_channel_with_at_most_sixteen_kib_an_entry() {
    let config = onboard_config("live");
    config.write(
        "layers/enroll.toml",
        "model = \"channel\"\nshapes = \"enroll.shapes.ttl\"\ncontext = \"enroll.context.jsonld\"\nlobby = true\nentry_max = \"1MiB\"\n",
    );
    let e = Config::load(config.path()).unwrap_err().to_string();
    assert!(e.contains("lobby") && e.contains("16384"), "{e}");

    config.write(
        "layers/enroll.toml",
        "model = \"tuplespace\"\nshapes = \"enroll.shapes.ttl\"\ncontext = \"enroll.context.jsonld\"\nlobby = true\n",
    );
    let e = Config::load(config.path()).unwrap_err().to_string();
    assert!(e.contains("`lobby` is a channel option"), "{e}");
}

#[test]
fn onboarding_names_its_layer_key_and_trust_directory() {
    for (table, why) in [
        (
            "[onboard]\ncontroller = \"brian\"\ncontroller_key = \"controller.pub.pem\"\ntrust_dir = \"t\"\n",
            "`onboard.grants` is required",
        ),
        (
            "[onboard]\ngrants = \"grants\"\ncontroller_key = \"controller.pub.pem\"\ntrust_dir = \"t\"\n",
            "`onboard.controller` is required",
        ),
        (
            "[onboard]\ngrants = \"grants\"\ncontroller = \"brian\"\ntrust_dir = \"t\"\n",
            "`onboard.controller_key` is required",
        ),
        (
            "[onboard]\ngrants = \"grants\"\ncontroller = \"brian\"\ncontroller_key = \"controller.pub.pem\"\n",
            "`onboard.trust_dir` is required",
        ),
        (
            "[onboard]\ngrants = \"enroll\"\ncontroller = \"brian\"\ncontroller_key = \"controller.pub.pem\"\ntrust_dir = \"t\"\n",
            "is not a channel (or is a lobby)",
        ),
        (
            "[onboard]\ngrants = \"grants\"\ncontroller = \"brian\"\ncontroller_key = \"../k.pem\"\ntrust_dir = \"t\"\n",
            "not a path",
        ),
        (
            "[onboard]\ngrants = \"grants\"\ncontroller = \"brian\"\ncontroller_key = \"participants.ttl\"\ntrust_dir = \"t\"\n",
            "not one PEM public key",
        ),
    ] {
        let config = onboard_config("live");
        config.write("layers.toml", table);
        let state = Scratch::new("onboard-state");
        let e = Host::open(config.path(), state.path(), &key(config.path()))
            .err()
            .unwrap_or_else(|| panic!("{table} opened"))
            .to_string();
        assert!(e.contains(why), "{table}: {e}");
    }
}
