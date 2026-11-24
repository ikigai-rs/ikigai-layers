//! `idempotency-key=` on `say` (ledger #4060, from the Sink idempotency research, ledger
//! #2770, ikigai-core `docs/research/sink-idempotency.md` section 5.2).
//!
//! The first test is that research's layers case, ported from its reproduction
//! (`docs/research/sink-idempotency/repro/`): a link that delivers the write and loses the
//! answer, and the retry a contract-respecting caller (`ikigai-throttle`'s `Retry`) makes.
//! Before this change the retry left two entries, since an append-only layer cannot remove a
//! duplicate later. Now it leaves one, and the retry's bytes equal the lost answer's.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use common::*;
use futures::executor::block_on;
use ikigai_core::{
    Capability, Error, Kernel, Representation, Request, Resend, Resolution, Scope, Space, Verb,
};

const KEY: &str = "idempotency-key";

fn bobby() -> String {
    participant("bobby")
}

fn chris() -> String {
    participant("chris")
}

/// A link that delivers every request, and loses the answer of one when told to.
struct LossyLink<'a> {
    kernel: &'a Kernel,
    lose: AtomicBool,
    lost: Mutex<Vec<Vec<u8>>>,
}

impl<'a> LossyLink<'a> {
    fn new(kernel: &'a Kernel) -> Self {
        LossyLink {
            kernel,
            lose: AtomicBool::new(false),
            lost: Mutex::new(Vec::new()),
        }
    }

    fn lose_next(&self) {
        self.lose.store(true, Ordering::SeqCst);
    }

    fn send(&self, request: Request, cap: &Capability) -> Result<Representation, Error> {
        let answer = block_on(self.kernel.issue(request, cap));
        if self.lose.swap(false, Ordering::SeqCst) {
            if let Ok(repr) = &answer {
                self.lost.lock().unwrap().push(repr.bytes.clone());
            }
            return Err(Error::Timeout(
                "the link dropped after the request was delivered".into(),
            ));
        }
        answer
    }

    fn last_lost(&self) -> Vec<u8> {
        self.lost
            .lock()
            .unwrap()
            .last()
            .expect("a lost answer")
            .clone()
    }
}

/// Whether the endpoint's contract says THIS call may be sent again.
fn resend_declared_safe(space: &dyn Space, request: &Request) -> bool {
    let meta = Request::new(Verb::Meta, request.target.clone());
    let Resolution::Hit(resolved) = space.resolve(&meta, &Scope::empty()) else {
        panic!("unresolved {}", request.target)
    };
    resolved
        .endpoint
        .describe()
        .action_specs()
        .into_iter()
        .find(|a| a.verb == request.verb)
        .is_some_and(|a| a.resend_is_safe(|name| request.args.contains_key(name)))
}

/// What `ikigai-throttle`'s `Retry` re-sends, and no more: a transient failure is sent again
/// only when the contract declares this call's resend safe.
fn contract_send(
    link: &LossyLink<'_>,
    space: &dyn Space,
    request: &Request,
    cap: &Capability,
) -> (Result<Representation, Error>, usize) {
    let safe = resend_declared_safe(space, request);
    let mut attempts = 0;
    loop {
        attempts += 1;
        match link.send(request.clone(), cap) {
            Err(e) if e.is_transient() && safe && attempts < 3 => continue,
            other => return (other, attempts),
        }
    }
}

fn keyed_say(layer: &str, content: &str, key: &str, extra: &[(&str, &str)]) -> Request {
    let mut args = vec![("content", content), (KEY, key)];
    args.extend_from_slice(extra);
    request(Verb::Sink, &format!("urn:layer:{layer}:say"), &args)
}

fn send(kernel: &Kernel, request: Request, cap: &Capability) -> Result<String, Error> {
    block_on(kernel.issue(request, cap)).map(|r| text(&r))
}

fn copies(kernel: &Kernel, layer: &str, body: &str, who: &str) -> usize {
    source(
        kernel,
        &format!("urn:layer:{layer}:entries"),
        &[],
        &member(who),
    )
    .matches(body)
    .count()
}

#[test]
fn a_lost_answer_retried_under_a_key_is_one_entry_and_the_first_answer() {
    let f = fixture();
    let space = root_over(&f.host);
    let kernel = kernel_from(space.clone());
    let link = LossyLink::new(&kernel);
    let say = keyed_say("talk", &message("repro-say-55d1"), "retry-7f3a", &[]);
    assert!(
        resend_declared_safe(&*space, &say),
        "say declares a resend safe under idempotency-key="
    );

    link.lose_next();
    let (answer, attempts) = contract_send(&link, &*space, &say, &member(&bobby()));
    let answer = answer.expect("the keyed retry succeeds");
    assert_eq!(attempts, 2);
    assert_eq!(
        answer.bytes,
        link.last_lost(),
        "the retry answers the lost answer byte for byte"
    );
    assert_eq!(copies(&kernel, "talk", "repro-say-55d1", &bobby()), 1);
    // And it was logged once.
    let feed = source(
        &kernel,
        "urn:layer:talk:feed",
        &[("after", "0")],
        &member(&bobby()),
    );
    assert_eq!(feed.lines().count(), 1, "{feed}");
}

#[test]
fn without_a_key_say_is_not_declared_resend_safe() {
    let f = fixture();
    let space = root_over(&f.host);
    let plain = request(
        Verb::Sink,
        "urn:layer:talk:say",
        &[("content", &message("plain"))],
    );
    assert!(!resend_declared_safe(&*space, &plain));
}

#[test]
fn the_contract_names_the_key_and_publishes_the_window() {
    let f = fixture();
    let space = root_over(&f.host);
    let meta = Request::new(
        Verb::Meta,
        ikigai_core::Iri::parse("urn:layer:talk:say").unwrap(),
    );
    let Resolution::Hit(resolved) = space.resolve(&meta, &Scope::empty()) else {
        panic!("unresolved say")
    };
    let description = resolved.endpoint.describe();
    description.validate().expect("a valid description");
    let sink = description
        .action_specs()
        .into_iter()
        .find(|a| a.verb == Verb::Sink)
        .expect("a Sink action");
    assert_eq!(sink.resend, Some(Resend::Key(KEY.into())));
    assert_eq!(sink.resend_window_secs, Some(24 * 60 * 60));
    assert!(sink.inputs.iter().any(|i| i.name == KEY));
}

#[test]
fn the_json_face_replays_byte_for_byte() {
    let f = fixture();
    let say = keyed_say(
        "talk",
        &message("json"),
        "k:json",
        &[("as", "application/json")],
    );
    let first = send(&f.kernel, say.clone(), &member(&bobby())).unwrap();
    let again = send(&f.kernel, say, &member(&bobby())).unwrap();
    assert_eq!(first, again);
    assert_eq!(copies(&f.kernel, "talk", "\"json\"", &bobby()), 1);
}

#[test]
fn a_key_reused_for_a_different_request_is_refused() {
    let f = fixture();
    let cap = member(&bobby());
    send(
        &f.kernel,
        keyed_say("talk", &message("first"), "k-1", &[]),
        &cap,
    )
    .unwrap();
    let refused = send(
        &f.kernel,
        keyed_say("talk", &message("second"), "k-1", &[]),
        &cap,
    );
    match refused {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, KEY);
            assert!(
                detail.contains("`k-1` was used for a different request"),
                "{detail}"
            );
        }
        other => panic!("expected a refused key, got {other:?}"),
    }
    // The same content in another face is a different request too.
    let other_face = send(
        &f.kernel,
        keyed_say(
            "talk",
            &message("first"),
            "k-1",
            &[("as", "application/json")],
        ),
        &cap,
    );
    assert!(
        matches!(other_face, Err(Error::InvalidArgument { ref name, .. }) if name == KEY),
        "{other_face:?}"
    );
    assert_eq!(copies(&f.kernel, "talk", "second", &bobby()), 0);
}

#[test]
fn a_key_is_scoped_to_its_principal_and_its_layer() {
    let f = fixture();
    let say = keyed_say("talk", &message("shared"), "k-same", &[]);
    let a = send(&f.kernel, say.clone(), &member(&bobby())).unwrap();
    let b = send(&f.kernel, say, &member(&chris())).unwrap();
    assert_ne!(a, b, "another principal's token is its own");
    assert_eq!(copies(&f.kernel, "talk", "shared", &bobby()), 2);
    // The same token and principal on another layer is another resource's.
    let c = send(
        &f.kernel,
        keyed_say("talk-bobby", &message("shared"), "k-same", &[]),
        &member(&bobby()),
    )
    .unwrap();
    assert!(c.starts_with("urn:layer:talk-bobby:entry:"), "{c}");
}

#[test]
fn the_record_survives_a_restart() {
    let mut f = fixture();
    let say = keyed_say("talk", &message("durable"), "k-durable", &[]);
    let first = send(&f.kernel, say.clone(), &member(&bobby())).unwrap();
    f.reopen();
    let again = send(&f.kernel, say, &member(&bobby())).unwrap();
    assert_eq!(first, again);
    assert_eq!(copies(&f.kernel, "talk", "durable", &bobby()), 1);
}

#[test]
fn the_record_is_not_part_of_the_entry() {
    let f = fixture();
    let iri = send(
        &f.kernel,
        keyed_say("talk", &message("quiet"), "k-quiet", &[]),
        &member(&bobby()),
    )
    .unwrap();
    let entry = source(&f.kernel, &iri, &[], &member(&chris()));
    assert!(!entry.contains("k-quiet"), "{entry}");
    let entries = source(&f.kernel, "urn:layer:talk:entries", &[], &member(&chris()));
    assert!(!entries.contains("k-quiet"), "{entries}");
}

#[test]
fn a_refused_say_records_nothing() {
    let f = fixture();
    let cap = member(&bobby());
    // Not admitted (no ik:body): InvalidArgument, before anything is stored.
    let bad = format!("@prefix ik: <{IK}> .\n<urn:layer:new> a ik:Message .\n");
    assert!(send(&f.kernel, keyed_say("talk", &bad, "k-r", &[]), &cap).is_err());
    // So the token is free for another request.
    send(
        &f.kernel,
        keyed_say("talk", &message("fine"), "k-r", &[]),
        &cap,
    )
    .unwrap();
    // A Denied one (a principal that is no participant) records nothing either.
    let stranger = cap_for_talk("urn:collab:participant:nobody");
    assert!(matches!(
        send(
            &f.kernel,
            keyed_say("talk", &message("x"), "k-d", &[]),
            &stranger
        ),
        Err(Error::Denied(_))
    ));
    send(
        &f.kernel,
        keyed_say("talk", &message("y"), "k-d", &[]),
        &cap,
    )
    .unwrap();
}

fn cap_for_talk(principal: &str) -> Capability {
    cap(
        principal,
        &[
            "urn:cap:layer:read:talk",
            "urn:cap:layer:feed:talk",
            "urn:cap:layer:say:talk",
        ],
    )
}

#[test]
fn a_malformed_key_is_refused_by_name() {
    let f = fixture();
    for bad in ["", "has space", "slash/no", &"k".repeat(257)] {
        match send(
            &f.kernel,
            keyed_say("talk", &message("m"), bad, &[]),
            &member(&bobby()),
        ) {
            Err(Error::InvalidArgument { name, .. }) => assert_eq!(name, KEY, "{bad:?}"),
            other => panic!("{bad:?}: expected a refused key, got {other:?}"),
        }
    }
    assert_eq!(copies(&f.kernel, "talk", "\"m\"", &bobby()), 0);
    send(
        &f.kernel,
        keyed_say("talk", &message("m"), &"k".repeat(256), &[]),
        &member(&bobby()),
    )
    .unwrap();
}

#[test]
fn a_key_after_its_window_is_a_new_request() {
    let f = fixture();
    let cap = member(&bobby());
    let say = keyed_say("talk", &message("late"), "k-late", &[]);
    let first = send(&f.kernel, say.clone(), &cap).unwrap();
    // A kernel whose clock reads a day and a second later.
    let later = ikigai_core::Kernel::with_meta_renderer(
        root_over(&f.host),
        std::sync::Arc::new(ikigai_vocab::TurtleRenderer),
    )
    .with_clock(std::sync::Arc::new(LaterClock));
    let second = send(&later, say, &cap).unwrap();
    assert_ne!(first, second);
    assert_eq!(copies(&f.kernel, "talk", "late", &bobby()), 2);
}

struct LaterClock;

impl ikigai_core::Clock for LaterClock {
    fn now(&self) -> ikigai_core::Time {
        // TickingClock starts at 2026-10-10T00:00:00Z; a test makes few writes.
        ikigai_core::Time::from_millis(1_791_590_400_000 + 24 * 3600 * 1000 + 60_000)
    }
}
