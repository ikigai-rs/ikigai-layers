//! The gateway between kernels (ledger #1370), proven ikigai-to-ikigai: an asker and its
//! resolvers on a scratch layer host, each resolver resolving in its OWN kernel, with the
//! shipped shapes and contexts. An HTTP front on the same layers will let a participant that is
//! not ikigai take either end; nothing here may depend on either side being ikigai except the
//! reference participant itself.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use common::*;
use futures::executor::block_on;
use ikigai_core::{
    Capability, Description, Endpoint, EndpointSpace, Error, Invocation, Iri, Kernel, ReprType,
    Representation, Result, UriTemplate, Verb,
};
use ikigai_layers::gateway::{
    self, Ask, Asker, Door, KernelDoor, Resolver, Served, CONTEXT, EVENT_CONTEXT, EVENT_SHAPES,
    SHAPES,
};
use ikigai_layers::grants::{generate, load_fingerprints};
use ikigai_layers::{Config, Host};

// ---- the fixture ------------------------------------------------------------------------

const LAYER: &str = "gateway-a";
const OTHER: &str = "gateway-b";
const EVENTS: &str = "gateway-events";

const ASK_LAYER: &str = "model = \"tuplespace\"\nshapes = \"gateway.shapes.ttl\"\ncontext = \"gateway.context.jsonld\"\naddressed = true\nentry_max = \"4KiB\"\n";

const MANIFEST: &str = r#"label = "Gateway"

# a asks; r1 and r2 resolve.
[[mount]]
layer = "gateway-a"
a  = ["read", "feed", "out", "take"]
r1 = ["read", "feed", "out", "take"]
r2 = ["read", "feed", "out", "take"]

# b asks; r1 resolves.
[[mount]]
layer = "gateway-b"
b  = ["read", "feed", "out", "take"]
r1 = ["read", "feed", "out", "take"]

# r1 owns the resources and says when they change.
[[mount]]
layer = "gateway-events"
r1 = ["read", "feed", "say"]
a  = ["read", "feed"]
b  = ["read", "feed"]
"#;

const PARTICIPANTS: &str = r#"@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:collab:participant:a> a ik:Machine .
<urn:collab:participant:b> a ik:Machine .
<urn:collab:participant:r1> a ik:Machine .
<urn:collab:participant:r2> a ik:Machine .
"#;

struct Gateway {
    _config: Scratch,
    _state: Scratch,
    _host: Arc<Host>,
    /// The layer host's kernel, as a door binds it.
    layers: Kernel,
    /// The resolvers' own kernel: the data they serve.
    data: Kernel,
    grants: std::collections::BTreeMap<String, Vec<String>>,
}

fn gateway_fixture() -> Gateway {
    let config = Scratch::new("gw-config");
    config.write("layers/gateway.shapes.ttl", SHAPES);
    config.write("layers/gateway.context.jsonld", CONTEXT);
    config.write("layers/gateway-events.shapes.ttl", EVENT_SHAPES);
    config.write("layers/gateway-events.context.jsonld", EVENT_CONTEXT);
    config.write(&format!("layers/{LAYER}.toml"), ASK_LAYER);
    config.write(&format!("layers/{OTHER}.toml"), ASK_LAYER);
    config.write(
        &format!("layers/{EVENTS}.toml"),
        "model = \"channel\"\nshapes = \"gateway-events.shapes.ttl\"\ncontext = \"gateway-events.context.jsonld\"\nabout = \"urn:layers:gateway:resource\"\n",
    );
    config.write("participants.ttl", PARTICIPANTS);
    config.write("collabs/gateway.toml", MANIFEST);
    let state = Scratch::new("gw-state");
    let host = Host::open(config.path(), state.path()).expect("the gateway configuration");
    let layers = kernel_over(&host);
    let loaded = Config::load(config.path()).unwrap();
    let fingerprints = load_fingerprints(config.path(), &loaded).unwrap();
    let grants = generate(&loaded, &fingerprints).unwrap().grants;
    Gateway {
        _config: config,
        _state: state,
        _host: host,
        layers,
        data: data_kernel(),
        grants,
    }
}

impl Gateway {
    /// What the door mints for `who`: the GENERATED grant, so the manifest is what is tested.
    fn grant(&self, who: &str) -> Capability {
        let scopes = self.grants.get(who).cloned().unwrap_or_default();
        Capability::scoped(scopes)
            .with_principal(&participant(who))
            .unwrap()
    }

    fn door(&self, who: &str) -> KernelDoor<'_> {
        KernelDoor::new(&self.layers, self.grant(who))
    }

    fn asker(&self, who: &str, layer: &str) -> Asker<KernelDoor<'_>> {
        Asker::new(self.door(who), layer)
    }

    /// A resolver over the data kernel, under the resolver OPERATOR's capability: data, not
    /// secrets.
    fn resolver(&self) -> Resolver<KernelDoor<'_>> {
        Resolver::new(KernelDoor::new(
            &self.data,
            Capability::scoped([DATA_CAP.to_string()]),
        ))
    }

    fn issue(&self, who: &str, verb: Verb, iri: &str, args: &[(&str, &str)]) -> Result<String> {
        issue(&self.layers, verb, iri, args, &self.grant(who)).map(|r| text(&r))
    }

    fn out(&self, who: &str, layer: &str, content: &str) -> Result<String> {
        self.issue(
            who,
            Verb::Sink,
            &format!("urn:layer:{layer}:tuples"),
            &[("content", content)],
        )
    }

    fn take(&self, who: &str, layer: &str, subject: &str) -> Result<String> {
        self.issue(
            who,
            Verb::Delete,
            &format!("urn:layer:{layer}:tuples"),
            &[("tuple", subject)],
        )
    }

    fn feed_kinds(&self, layer: &str) -> Vec<String> {
        self.issue(
            "r1",
            Verb::Source,
            &format!("urn:layer:{layer}:feed"),
            &[("after", "0")],
        )
        .unwrap()
        .lines()
        .map(|l| l.split(' ').nth(2).unwrap().to_string())
        .collect()
    }

    /// Run `body` with resolver `who` following `layer` on its own thread.
    fn with_resolver<T>(&self, who: &str, layer: &str, body: impl FnOnce() -> T) -> T {
        let stop = AtomicBool::new(false);
        let door = self.door(who);
        let resolver = self.resolver();
        std::thread::scope(|s| {
            s.spawn(|| {
                let mut cursor = 0;
                while !stop.load(Ordering::SeqCst) {
                    cursor = block_on(gateway::follow(
                        &door,
                        layer,
                        &resolver,
                        cursor,
                        Duration::from_secs(1),
                    ))
                    .expect("the resolver follows the layer");
                }
            });
            let answer = body();
            stop.store(true, Ordering::SeqCst);
            answer
        })
    }
}

/// The capability the data kernel's resources require.
const DATA_CAP: &str = "urn:cap:data:read";

/// `urn:data:{name}`: a fixed text per name, absent for `missing`; Exists answers whether it
/// is there; Sink would store (and so is a MUTATING verb a resolver must not run by default).
struct Data;

#[async_trait]
impl Endpoint for Data {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let name = inv.bindings.get("name").unwrap_or_default();
        let text = |t: String| Representation::new(ReprType::new("text/plain"), t.into_bytes());
        match inv.request.verb {
            Verb::Source if name == "missing" => Err(Error::NotFound(format!("no datum `{name}`"))),
            Verb::Source => {
                let shout = inv
                    .request
                    .args
                    .get("shout")
                    .map(|_| inv.inline_str("shout").unwrap_or_default() == "yes")
                    .unwrap_or(false);
                let value = format!("the value of {name}");
                Ok(text(if shout { value.to_uppercase() } else { value }))
            }
            Verb::Exists => Ok(text((name != "missing").to_string())),
            Verb::Sink => Ok(text("stored".into())),
            other => Err(Error::Endpoint(format!("data: no {other:?}"))),
        }
    }

    fn name(&self) -> &str {
        "data"
    }

    fn describe(&self) -> Description {
        Description::new("data")
            .verb(Verb::Source)
            .verb(Verb::Exists)
            .verb(Verb::Sink)
            .requires(DATA_CAP)
    }
}

/// `urn:secret:{name}`: requires a capability the resolver's operator did not give it.
struct Secret;

#[async_trait]
impl Endpoint for Secret {
    async fn invoke(&self, _inv: &Invocation<'_>) -> Result<Representation> {
        Ok(Representation::new(
            ReprType::new("text/plain"),
            b"the secret".to_vec(),
        ))
    }

    fn name(&self) -> &str {
        "secret"
    }

    fn describe(&self) -> Description {
        Description::new("secret")
            .verb(Verb::Source)
            .requires("urn:cap:secret:read")
    }
}

fn data_kernel() -> Kernel {
    let space = EndpointSpace::new()
        .bind(UriTemplate::parse("urn:data:{name}").unwrap(), Data)
        .bind(UriTemplate::parse("urn:secret:{name}").unwrap(), Secret);
    Kernel::new(Arc::new(space))
}

fn iri(text: &str) -> Iri {
    Iri::parse(text).unwrap()
}

const DEADLINE: Duration = Duration::from_secs(20);

// ---- across the gateway -----------------------------------------------------------------

#[test]
fn source_crosses_the_gateway_resolved_in_the_resolvers_kernel() {
    let g = gateway_fixture();
    let asker = g.asker("a", LAYER);
    let answer = g.with_resolver("r1", LAYER, || {
        block_on(asker.ask(
            &Ask::new(Verb::Source, iri("urn:data:x")).arg("shout", "yes"),
            DEADLINE,
        ))
    });
    let answer = answer.unwrap();
    assert_eq!(answer.repr_type.media_type, "text/plain");
    assert_eq!(text(&answer), "THE VALUE OF X");
    // The layer recorded the exchange: the request out, its take, the reply out, its take.
    assert_eq!(g.feed_kinds(LAYER), ["out", "taken", "out", "taken"]);
    // Who took the request and who took the reply, from the receipts.
    let receipts = g
        .issue(
            "a",
            Verb::Source,
            "urn:layer:gateway-a:tuples",
            &[("state", "taken")],
        )
        .unwrap();
    let lines: Vec<&str> = receipts.lines().collect();
    assert!(lines[0].contains(":request:") && lines[0].ends_with(&participant("r1")));
    assert!(lines[1].contains(":reply:") && lines[1].ends_with(&participant("a")));
}

#[test]
fn exists_crosses_the_gateway() {
    let g = gateway_fixture();
    let asker = g.asker("a", LAYER);
    let (present, absent) = g.with_resolver("r1", LAYER, || {
        (
            block_on(asker.ask(&Ask::new(Verb::Exists, iri("urn:data:x")), DEADLINE)),
            block_on(asker.ask(&Ask::new(Verb::Exists, iri("urn:data:missing")), DEADLINE)),
        )
    });
    assert_eq!(text(&present.unwrap()), "true");
    assert_eq!(text(&absent.unwrap()), "false");
}

#[test]
fn not_found_and_denied_come_back_typed() {
    let g = gateway_fixture();
    let asker = g.asker("a", LAYER);
    let ask = |verb, name: &str| block_on(asker.ask(&Ask::new(verb, iri(name)), DEADLINE));
    let (missing, secret, unbound, sink) = g.with_resolver("r1", LAYER, || {
        (
            ask(Verb::Source, "urn:data:missing"),
            ask(Verb::Source, "urn:secret:x"),
            ask(Verb::Source, "urn:nothing:x"),
            ask(Verb::Sink, "urn:data:x"),
        )
    });
    // The resolver's kernel's own NotFound, message and all.
    match missing {
        Err(Error::NotFound(m)) => assert_eq!(m, "no datum `missing`"),
        other => panic!("{other:?}"),
    }
    // Denied by the resolver's KERNEL: its operator's capability does not reach secrets,
    // whatever the asker holds.
    match secret {
        Err(Error::Denied(m)) => assert!(m.contains("urn:cap:secret:read"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Nothing bound there: the kernel's own Unresolved, naming the name.
    match unbound {
        Err(Error::Unresolved(name)) => assert_eq!(name.as_str(), "urn:nothing:x"),
        other => panic!("{other:?}"),
    }
    // Denied by the RESOLVER: a mutating verb is not answered unless its operator allows it.
    match sink {
        Err(Error::Denied(m)) => assert!(m.contains("does not answer Sink"), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_resolver_answers_only_what_its_operator_allows() {
    let g = gateway_fixture();
    let asker = g.asker("a", LAYER);
    let door = g.door("r1");
    let resolver = Resolver::new(KernelDoor::new(
        &g.data,
        Capability::scoped([DATA_CAP.to_string()]),
    ))
    .verbs([Verb::Source, Verb::Sink])
    .prefixes(["urn:data:"]);
    let stop = AtomicBool::new(false);
    let (sink, outside) = std::thread::scope(|s| {
        s.spawn(|| {
            let mut cursor = 0;
            while !stop.load(Ordering::SeqCst) {
                cursor = block_on(gateway::follow(
                    &door,
                    LAYER,
                    &resolver,
                    cursor,
                    Duration::from_secs(1),
                ))
                .unwrap();
            }
        });
        let answers = (
            block_on(asker.ask(
                &Ask::new(Verb::Sink, iri("urn:data:x")).arg("content", "v"),
                DEADLINE,
            )),
            block_on(asker.ask(&Ask::new(Verb::Source, iri("urn:secret:x")), DEADLINE)),
        );
        stop.store(true, Ordering::SeqCst);
        answers
    });
    assert_eq!(text(&sink.unwrap()), "stored");
    match outside {
        Err(Error::Denied(m)) => assert!(m.contains("names under urn:data:"), "{m}"),
        other => panic!("{other:?}"),
    }
}

// ---- admission --------------------------------------------------------------------------

#[test]
fn a_request_the_shapes_refuse_is_refused_at_admission() {
    let g = gateway_fixture();
    let prefix = "@prefix gw: <urn:layers:gateway:> .\n@prefix ik: <https://ikigai-rs.dev/ns#> .\n";
    let subject = "<urn:layer:gateway-a:request:c1>";
    for (why, body) in [
        (
            "a verb that is not one of the five",
            "a gw:Request ; gw:correlation \"c1\" ; gw:resource <urn:data:x> ; ik:verb \"Fetch\" .",
        ),
        (
            "no correlation id",
            "a gw:Request ; gw:resource <urn:data:x> ; ik:verb \"Source\" .",
        ),
        (
            "a correlation id the pattern refuses",
            "a gw:Request ; gw:correlation \"c 1\" ; gw:resource <urn:data:x> ; ik:verb \"Source\" .",
        ),
        (
            "no resource",
            "a gw:Request ; gw:correlation \"c1\" ; ik:verb \"Source\" .",
        ),
        (
            "an argument that is not name=value",
            "a gw:Request ; gw:correlation \"c1\" ; gw:resource <urn:data:x> ; ik:verb \"Source\" ; gw:argument \"=v\" .",
        ),
        (
            "a property the closed shape does not name",
            "a gw:Request ; gw:correlation \"c1\" ; gw:resource <urn:data:x> ; ik:verb \"Source\" ; gw:priority \"high\" .",
        ),
    ] {
        match g.out("a", LAYER, &format!("{prefix}{subject} {body}")) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "content", "{why}");
                assert!(detail.contains("does not conform"), "{why}: {detail}");
            }
            other => panic!("{why}: {other:?}"),
        }
    }
    // A class no shape targets is refused before the shapes run.
    match g.out(
        "a",
        LAYER,
        &format!("{prefix}{subject} a gw:Question ; gw:correlation \"c1\" ."),
    ) {
        Err(Error::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("admits only"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    // Nothing refused is logged.
    assert!(g.feed_kinds(LAYER).is_empty());
}

/// What a remote participant will send through an HTTP front: JSON-LD under the layer's
/// own context. Admitted exactly as Turtle is, and served the same.
#[test]
fn a_json_ld_request_under_the_layer_context_is_admitted_and_answered() {
    let g = gateway_fixture();
    let context: serde_json::Value = serde_json::from_str(CONTEXT).unwrap();
    let request = serde_json::json!({
        "@context": context["@context"],
        "@id": "urn:layer:gateway-a:request:j1",
        "@type": "Request",
        "correlation": "j1",
        "resource": "urn:data:j",
        "verb": "Source",
        "argument": ["shout=yes"]
    });
    assert_eq!(
        g.out("a", LAYER, &request.to_string()).unwrap(),
        "urn:layer:gateway-a:request:j1"
    );
    let served = block_on(gateway::serve(
        &g.door("r1"),
        LAYER,
        &g.resolver(),
        "urn:layer:gateway-a:request:j1",
    ))
    .unwrap();
    assert_eq!(
        served,
        Served::Answered("urn:layer:gateway-a:reply:j1".into())
    );
    let reply = g.take("a", LAYER, "urn:layer:gateway-a:reply:j1").unwrap();
    let answer = gateway::parse_reply(LAYER, "j1", &reply).unwrap().unwrap();
    assert_eq!(text(&answer), "THE VALUE OF J");
}

// ---- who sees what ----------------------------------------------------------------------

#[test]
fn a_reply_to_someone_else_is_not_readable() {
    let g = gateway_fixture();
    let ask = Ask::new(Verb::Source, iri("urn:data:x"));
    g.out(
        "a",
        LAYER,
        &gateway::request_turtle(LAYER, "c1", &ask).unwrap(),
    )
    .unwrap();
    let request = gateway::request_subject(LAYER, "c1");
    let reply = gateway::reply_subject(LAYER, "c1");

    // r2 is a resolver of this layer too, but r1 took the request: r2 cannot answer it.
    let taken = g.take("r1", LAYER, &request).unwrap();
    let asked = gateway::parse_request(LAYER, &taken).unwrap();
    assert_eq!(asked.asker, participant("a"));
    let forged = gateway::reply_turtle(
        LAYER,
        &asked,
        &Ok(Representation::new(
            ReprType::new("text/plain"),
            b"forged".to_vec(),
        )),
    );
    match g.out("r2", LAYER, &forged) {
        Err(Error::Denied(m)) => assert!(m.contains("only its taker reports"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Nor can r1 address its answer to anyone but the asker.
    let misaddressed = gateway::reply_turtle(
        LAYER,
        &gateway::Asked {
            asker: participant("r2"),
            ..asked.clone()
        },
        &Ok(Representation::new(
            ReprType::new("text/plain"),
            b"x".to_vec(),
        )),
    );
    match g.out("r1", LAYER, &misaddressed) {
        Err(Error::InvalidArgument { detail, .. }) => {
            assert!(detail.contains("addressed to its offerer"), "{detail}")
        }
        other => panic!("{other:?}"),
    }
    let answer = block_on(g.resolver().resolve(&asked));
    assert_eq!(
        g.out("r1", LAYER, &gateway::reply_turtle(LAYER, &asked, &answer))
            .unwrap(),
        reply
    );

    // Another asker holds nothing on this layer: it can neither list nor take the reply.
    for (verb, args) in [
        (Verb::Source, vec![("tuple", reply.as_str())]),
        (Verb::Delete, vec![("tuple", reply.as_str())]),
    ] {
        match g.issue("b", verb, "urn:layer:gateway-a:tuples", &args) {
            Err(Error::Denied(m)) => assert!(m.contains("urn:cap:layer:"), "{verb:?}: {m}"),
            other => panic!("{verb:?}: {other:?}"),
        }
    }
    // A resolver of this layer can see it (the design says why: a resolver serves the layer),
    // but cannot take it: it is addressed to its asker.
    match g.take("r2", LAYER, &reply) {
        Err(Error::Denied(m)) => assert!(m.contains("only an addressee takes it"), "{m}"),
        other => panic!("{other:?}"),
    }
    // Only the asker takes it, once.
    let answer = g.take("a", LAYER, &reply).unwrap();
    assert_eq!(
        text(&gateway::parse_reply(LAYER, "c1", &answer).unwrap().unwrap()),
        "the value of x"
    );
    assert!(matches!(
        g.take("a", LAYER, &reply),
        Err(Error::Conflict(_))
    ));
}

#[test]
fn each_asker_sees_only_its_own_layer() {
    let g = gateway_fixture();
    // The generated grants: an asker holds its own ask layer and reads the events; a
    // resolver holds every ask layer it serves.
    let scopes = |who: &str| g.grants.get(who).cloned().unwrap_or_default();
    assert!(scopes("a").iter().all(|s| !s.ends_with(":gateway-b")));
    assert!(scopes("b").iter().all(|s| !s.ends_with(":gateway-a")));
    assert!(scopes("r1").contains(&"urn:cap:layer:take:gateway-b".to_string()));
    assert!(!scopes("r2").iter().any(|s| s.ends_with(":gateway-b")));
    // b's request is invisible to a.
    g.out(
        "b",
        OTHER,
        &gateway::request_turtle(OTHER, "c1", &Ask::new(Verb::Source, iri("urn:data:b"))).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        g.issue("a", Verb::Source, "urn:layer:gateway-b:tuples", &[]),
        Err(Error::Denied(_))
    ));
    assert!(matches!(
        g.issue(
            "a",
            Verb::Source,
            "urn:layer:gateway-b:feed",
            &[("after", "0")]
        ),
        Err(Error::Denied(_))
    ));
    // And the one wait over several feeds offers a only its own.
    assert!(matches!(
        g.issue(
            "a",
            Verb::Source,
            "urn:layers:feed",
            &[("after", "gateway-b:0")]
        ),
        Err(Error::Denied(_))
    ));
}

// ---- deadlines and races ----------------------------------------------------------------

#[test]
fn a_request_nobody_takes_times_out_at_the_deadline_and_is_withdrawn() {
    let g = gateway_fixture();
    let asker = g.asker("a", LAYER);
    let deadline = Duration::from_millis(400);
    let started = Instant::now();
    let answer = block_on(asker.ask_as(
        "lonely",
        &Ask::new(Verb::Source, iri("urn:data:x")),
        deadline,
    ));
    let waited = started.elapsed();
    match answer {
        Err(Error::Timeout(m)) => assert!(m.contains("withdrawn"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(
        waited >= deadline,
        "answered at {waited:?}, before the deadline"
    );
    assert!(
        waited < deadline + Duration::from_secs(3),
        "answered at {waited:?}"
    );
    // Withdrawn: nothing pending, and the receipt says the asker took its own request back,
    // so no resolver can run it late.
    assert_eq!(
        g.issue("r1", Verb::Source, "urn:layer:gateway-a:tuples", &[])
            .unwrap(),
        ""
    );
    let late = block_on(gateway::serve(
        &g.door("r1"),
        LAYER,
        &g.resolver(),
        &gateway::request_subject(LAYER, "lonely"),
    ))
    .unwrap();
    match late {
        Served::Lost(m) => assert!(m.contains(&participant("a")), "{m}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn two_resolvers_racing_for_one_request_exactly_one_takes_it() {
    for round in 0..5 {
        let g = gateway_fixture();
        let correlation = format!("race{round}");
        g.out(
            "a",
            LAYER,
            &gateway::request_turtle(
                LAYER,
                &correlation,
                &Ask::new(Verb::Source, iri("urn:data:r")),
            )
            .unwrap(),
        )
        .unwrap();
        let request = gateway::request_subject(LAYER, &correlation);
        let barrier = Barrier::new(2);
        let results: Vec<(String, Served)> = std::thread::scope(|s| {
            let racers: Vec<_> = ["r1", "r2"]
                .into_iter()
                .map(|who| {
                    let (barrier, request, g) = (&barrier, &request, &g);
                    s.spawn(move || {
                        let door = g.door(who);
                        let resolver = g.resolver();
                        barrier.wait();
                        (
                            who.to_string(),
                            block_on(gateway::serve(&door, LAYER, &resolver, request)).unwrap(),
                        )
                    })
                })
                .collect();
            racers.into_iter().map(|r| r.join().unwrap()).collect()
        });
        let winners: Vec<&String> = results
            .iter()
            .filter(|(_, s)| matches!(s, Served::Answered(_)))
            .map(|(who, _)| who)
            .collect();
        assert_eq!(winners.len(), 1, "{results:?}");
        let winner = participant(winners[0]);
        let loser = results
            .iter()
            .find_map(|(_, s)| match s {
                Served::Lost(m) => Some(m.clone()),
                _ => None,
            })
            .expect("the other lost");
        assert!(loser.contains(&winner), "{loser}");
        // One take, one answer.
        assert_eq!(g.feed_kinds(LAYER), ["out", "taken", "out"]);
        let reply = g
            .take("a", LAYER, &gateway::reply_subject(LAYER, &correlation))
            .unwrap();
        assert!(reply.contains(&winner));
    }
}

#[test]
fn an_answer_too_large_for_the_layer_crosses_as_a_failure() {
    let g = gateway_fixture();
    // The data kernel answers a 4 KiB layer's worth and more for a long name.
    let long = "x".repeat(3000);
    let asker = g.asker("a", LAYER);
    let answer = g.with_resolver("r1", LAYER, || {
        block_on(asker.ask(
            &Ask::new(Verb::Source, iri(&format!("urn:data:{long}"))),
            DEADLINE,
        ))
    });
    match answer {
        Err(Error::Endpoint(m)) => assert!(m.contains("could not cross layer `gateway-a`"), "{m}"),
        other => panic!("{other:?}"),
    }
}

// ---- events -----------------------------------------------------------------------------

#[test]
fn the_owner_says_an_invalidation_and_askers_read_it() {
    let g = gateway_fixture();
    let invalidation = gateway::invalidation_turtle(&iri("urn:data:x"), "rewritten by its owner");
    let entry = g
        .issue(
            "r1",
            Verb::Sink,
            "urn:layer:gateway-events:say",
            &[("content", &invalidation)],
        )
        .unwrap();
    assert!(entry.starts_with("urn:layer:gateway-events:entry:"));
    let latest = g
        .issue(
            "a",
            Verb::Source,
            "urn:layer:gateway-events:latest",
            &[("about", "urn:data:x")],
        )
        .unwrap();
    let triples = triples(&latest);
    let has = |p: &str, o: &str| {
        triples
            .iter()
            .any(|t| t.predicate.as_str() == p && t.object.to_string() == o)
    };
    assert!(has(
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
        "<urn:layers:gateway:Invalidation>"
    ));
    assert!(has(
        "urn:layers:gateway:reason",
        "\"rewritten by its owner\""
    ));
    assert!(has(
        "http://www.w3.org/ns/prov#wasAttributedTo",
        &format!("<{}>", participant("r1"))
    ));
    // An asker cannot invalidate the owner's resources.
    assert!(matches!(
        g.issue(
            "a",
            Verb::Sink,
            "urn:layer:gateway-events:say",
            &[("content", &invalidation)]
        ),
        Err(Error::Denied(_))
    ));
    // And an invalidation needs its reason.
    let bare = "@prefix gw: <urn:layers:gateway:> .\n<urn:layer:new> a gw:Invalidation ; gw:resource <urn:data:x> .";
    match g.issue(
        "r1",
        Verb::Sink,
        "urn:layer:gateway-events:say",
        &[("content", bare)],
    ) {
        Err(Error::InvalidArgument { detail, .. }) => assert!(detail.contains("does not conform")),
        other => panic!("{other:?}"),
    }
}

// ---- the shipped example ----------------------------------------------------------------

/// The example configuration in `gateway/` opens as a host, and its grants keep each kernel's
/// say to its own event channel.
#[test]
fn the_shipped_example_configuration_opens() {
    let config = Scratch::new("gw-example");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("gateway");
    for rel in [
        "participants.ttl",
        "collabs/gateway.toml",
        "layers/gateway.shapes.ttl",
        "layers/gateway.context.jsonld",
        "layers/gateway-events.shapes.ttl",
        "layers/gateway-events.context.jsonld",
        "layers/gateway-plasma.toml",
        "layers/gateway-remote.toml",
        "layers/gateway-events-plasma.toml",
        "layers/gateway-events-remote.toml",
    ] {
        config.write(rel, &std::fs::read_to_string(root.join(rel)).unwrap());
    }
    let entries = std::fs::read_dir(root.join("layers")).unwrap().count();
    assert_eq!(
        entries, 8,
        "a file in gateway/layers this test does not copy"
    );
    let state = Scratch::new("gw-example-state");
    let host = Host::open(config.path(), state.path()).expect("the example opens");
    let names: Vec<&String> = host.layers().keys().collect();
    assert_eq!(
        names,
        [
            "gateway-events-plasma",
            "gateway-events-remote",
            "gateway-plasma",
            "gateway-remote"
        ]
    );
    let loaded = Config::load(config.path()).unwrap();
    let grants = generate(&loaded, &load_fingerprints(config.path(), &loaded).unwrap())
        .unwrap()
        .grants;
    let plasma = &grants["plasma"];
    assert!(plasma.contains(&"urn:cap:layer:say:gateway-events-plasma".to_string()));
    assert!(!plasma.contains(&"urn:cap:layer:say:gateway-events-remote".to_string()));
    let remote = &grants["remote"];
    assert!(remote.contains(&"urn:cap:layer:say:gateway-events-remote".to_string()));
    assert!(!remote.contains(&"urn:cap:layer:say:gateway-events-plasma".to_string()));
    // The shipped files ARE the module's constants.
    for (rel, constant) in [
        ("layers/gateway.shapes.ttl", SHAPES),
        ("layers/gateway.context.jsonld", CONTEXT),
        ("layers/gateway-events.shapes.ttl", EVENT_SHAPES),
        ("layers/gateway-events.context.jsonld", EVENT_CONTEXT),
    ] {
        assert_eq!(std::fs::read_to_string(root.join(rel)).unwrap(), constant);
    }
}

/// A door is all a participant needs: the same asker works through any of them (here, one
/// that counts its calls).
#[test]
fn an_asker_works_through_any_door() {
    struct Counting<'k>(KernelDoor<'k>, std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl Door for Counting<'_> {
        async fn call(&self, request: ikigai_core::Request) -> Result<Representation> {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.call(request).await
        }
    }
    let g = gateway_fixture();
    let asker = Asker::new(
        Counting(g.door("a"), std::sync::atomic::AtomicUsize::new(0)),
        LAYER,
    );
    let answer = g.with_resolver("r1", LAYER, || {
        block_on(asker.ask(&Ask::new(Verb::Source, iri("urn:data:d")), DEADLINE))
    });
    assert_eq!(text(&answer.unwrap()), "the value of d");
}

/// A request addressed to one resolver is that resolver's alone: another is turned away, and
/// an asker whose deadline passes cannot withdraw it (only an addressee takes it).
#[test]
fn a_request_addressed_to_one_resolver_is_its_alone() {
    let g = gateway_fixture();
    let asker = g.asker("a", LAYER);
    let ask = Ask::new(Verb::Source, iri("urn:data:x")).to(participant("r2"));
    match block_on(asker.ask_as("to-r2", &ask, Duration::from_millis(300))) {
        Err(Error::Timeout(m)) => assert!(m.contains("cannot be withdrawn"), "{m}"),
        other => panic!("{other:?}"),
    }
    let request = gateway::request_subject(LAYER, "to-r2");
    let r1 = block_on(gateway::serve(
        &g.door("r1"),
        LAYER,
        &g.resolver(),
        &request,
    ))
    .unwrap();
    assert!(matches!(r1, Served::NotOurs(_)), "{r1:?}");
    let r2 = block_on(gateway::serve(
        &g.door("r2"),
        LAYER,
        &g.resolver(),
        &request,
    ))
    .unwrap();
    assert_eq!(r2, Served::Answered(gateway::reply_subject(LAYER, "to-r2")));
    // The late answer waits, addressed to the asker, and asking again under the same id is
    // the same request (the layer's replay rule): its answer is taken at once.
    let answer = block_on(asker.ask_as("to-r2", &ask, Duration::from_millis(300))).unwrap();
    assert_eq!(text(&answer), "the value of x");
}

// ---- an asker that reads only JSON --------------------------------------------------------

/// An asker with nothing but a JSON parser (ledger #1371): it reads the layer's context as
/// JSON, asks in JSON-LD, waits on the feed's JSON, and reads its answer from the JSON-LD
/// face, `serde_json` alone, no RDF parser. The reference resolver serves it unchanged.
struct JsonAsker<'g> {
    g: &'g Gateway,
    who: &'static str,
    layer: &'static str,
}

impl JsonAsker<'_> {
    fn call(&self, verb: Verb, what: &str, args: &[(&str, &str)]) -> Result<serde_json::Value> {
        let repr = issue(
            &self.g.layers,
            verb,
            &format!("urn:layer:{}:{what}", self.layer),
            args,
            &self.g.grant(self.who),
        )?;
        Ok(serde_json::from_slice(&repr.bytes).expect("every answer read here is JSON"))
    }

    /// Ask `resource` under `correlation`, and wait for the answer: the reply node's JSON.
    fn ask(&self, correlation: &str, resource: &str, args: &[&str]) -> serde_json::Value {
        let context = self.call(Verb::Source, "context", &[]).unwrap();
        let subject = format!("urn:layer:{}:request:{correlation}", self.layer);
        let request = serde_json::json!({
            "@context": context["@context"],
            "@id": subject,
            "@type": "Request",
            "correlation": correlation,
            "resource": resource,
            "verb": "Source",
            "argument": args,
        });
        let asked = self
            .call(
                Verb::Sink,
                "tuples",
                &[
                    ("content", &request.to_string()),
                    ("as", "application/json"),
                ],
            )
            .unwrap();
        assert_eq!(asked, serde_json::json!({ "@id": subject }));
        let reply = format!("urn:layer:{}:reply:{correlation}", self.layer);
        let deadline = Instant::now() + DEADLINE;
        let mut cursor = 0u64;
        loop {
            match self.call(
                Verb::Delete,
                "tuples",
                &[("tuple", &reply), ("as", "application/ld+json")],
            ) {
                Ok(doc) => return reply_node(&doc, &reply),
                Err(Error::NotFound(_)) => {}
                Err(e) => panic!("taking <{reply}>: {e}"),
            }
            assert!(Instant::now() < deadline, "no answer to <{subject}>");
            let feed = self
                .call(
                    Verb::Source,
                    "feed",
                    &[
                        ("after", &cursor.to_string()),
                        ("wait", "1"),
                        ("as", "application/json"),
                    ],
                )
                .unwrap();
            cursor = feed["last"].as_u64().expect("the feed's `last`");
        }
    }
}

/// The node `id` of a JSON-LD answer's `@graph`.
fn reply_node(doc: &serde_json::Value, id: &str) -> serde_json::Value {
    doc["@graph"]
        .as_array()
        .expect("a top-level @graph array")
        .iter()
        .find(|n| n["@id"] == id)
        .unwrap_or_else(|| panic!("no <{id}> in {doc}"))
        .clone()
}

fn bytes_of(reply: &serde_json::Value) -> String {
    use base64::Engine;
    let encoded = reply["bytes"]
        .as_str()
        .expect("bytes, as their base64 text");
    String::from_utf8(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .expect("base64"),
    )
    .unwrap()
}

#[test]
fn an_asker_reading_only_json_crosses_the_gateway() {
    let g = gateway_fixture();
    let asker = JsonAsker {
        g: &g,
        who: "a",
        layer: LAYER,
    };
    let (ok, failed) = g.with_resolver("r1", LAYER, || {
        (
            asker.ask("json1", "urn:data:x", &["shout=yes"]),
            asker.ask("json2", "urn:data:missing", &[]),
        )
    });
    // A representation: its media type and its bytes.
    assert_eq!(ok["@type"], "Reply");
    assert_eq!(ok["mediaType"], "text/plain");
    assert_eq!(bytes_of(&ok), "THE VALUE OF X");
    assert_eq!(ok["offer"], gateway::request_subject(LAYER, "json1"));
    assert_eq!(ok["addressedTo"], participant("a"));
    assert_eq!(ok["correlation"], "json1");
    // The host's stamps: who answered, when, and where in the feed, as strings.
    assert_eq!(ok["attributedTo"], participant("r1"));
    assert!(ok["created"].is_string() && ok["seq"].is_string(), "{ok}");
    // A typed error: its kind and message.
    assert_eq!(failed["@type"], "Failure");
    assert_eq!(failed["errorKind"], "not-found");
    assert!(
        failed["message"].as_str().unwrap().contains("missing"),
        "{failed}"
    );

    // A take whose reply was lost: the second take is a Conflict, and the answer is read
    // back, in JSON, from the receipt that holds it.
    let reply = gateway::reply_subject(LAYER, "json1");
    match asker.call(
        Verb::Delete,
        "tuples",
        &[("tuple", &reply), ("as", "application/ld+json")],
    ) {
        Err(Error::Conflict(m)) => assert!(m.contains(&participant("a")), "{m}"),
        other => panic!("{other:?}"),
    }
    let receipt = asker
        .call(
            Verb::Source,
            "tuples",
            &[
                ("state", "taken"),
                ("tuple", &reply),
                ("as", "application/ld+json"),
            ],
        )
        .unwrap();
    let activity = receipt["@graph"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["@type"] == "Activity")
        .unwrap_or_else(|| panic!("no Activity in {receipt}"))
        .clone();
    assert_eq!(activity["used"], reply);
    assert_eq!(activity["associatedWith"], participant("a"));
    assert_eq!(bytes_of(&reply_node(&receipt, &reply)), "THE VALUE OF X");
}
