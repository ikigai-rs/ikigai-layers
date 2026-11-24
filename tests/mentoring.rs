//! The mentoring layers' admission checks (ledger #4274, from `docs/design/mentoring.md`): a
//! reply must name an existing entry of the right class (`[[reference]]`), a review receipt's
//! `result=` an entry its taker wrote (`[result]`), and an approval an answer someone else
//! wrote (`independent = true`). Every check reads under the WRITER's grants.
#![cfg(feature = "host")]
mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use common::*;
use ikigai_core::{Capability, Error, Time, Verb};

const MINUTE: u64 = 60_000;
const START: u64 = 1_791_590_400_000;
const M: &str = "urn:layers:mentoring:";

struct ManualClock(AtomicU64);
impl ikigai_core::Clock for ManualClock {
    fn now(&self) -> Time {
        Time::from_millis(self.0.load(Ordering::SeqCst))
    }
}

const PREFIXES: &str = "@prefix m: <urn:layers:mentoring:> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
";

const STAMPS: &str = "
        [ sh:path prov:wasAttributedTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path dcterms:created ; sh:datatype xsd:dateTime ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:seq ; sh:datatype xsd:integer ; sh:minCount 1 ; sh:maxCount 1 ] .
";

/// The design's question channel shapes: a `Question`, or an `Answer` replying to one.
fn question_shapes() -> String {
    format!(
        "{PREFIXES}
m:QuestionShape a sh:NodeShape ; sh:targetClass m:Question ; sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:Question ; sh:maxCount 1 ],
        [ sh:path ik:body ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ],{STAMPS}
m:AnswerShape a sh:NodeShape ; sh:targetClass m:Answer ; sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:Answer ; sh:maxCount 1 ],
        [ sh:path ik:body ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:inReplyTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ;
          sh:pattern \"^urn:layer:question:entry:[0-9a-hjkmnp-tv-z]{{23}}$\" ],
        [ sh:path m:receipt ; sh:nodeKind sh:IRI ; sh:maxCount 1 ],{STAMPS}"
    )
}

fn offer_shapes() -> String {
    format!(
        "{PREFIXES}
m:ReviewOfferShape a sh:NodeShape ; sh:targetClass m:ReviewOffer ; sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:ReviewOffer ; sh:maxCount 1 ],
        [ sh:path m:question ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path m:artifact ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:brief ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ],{STAMPS}"
    )
}

fn approval_shapes() -> String {
    format!(
        "{PREFIXES}
m:AnswerApprovalShape a sh:NodeShape ; sh:targetClass m:AnswerApproval ; sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:AnswerApproval ; sh:maxCount 1 ],
        [ sh:path m:answer ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ;
          sh:pattern \"^urn:layer:question:entry:[0-9a-hjkmnp-tv-z]{{23}}$\" ],{STAMPS}"
    )
}

const MENTORING_CONTEXT: &str = r#"{ "@context": {
  "Question": "urn:layers:mentoring:Question",
  "Answer": "urn:layers:mentoring:Answer",
  "ReviewOffer": "urn:layers:mentoring:ReviewOffer",
  "AnswerApproval": "urn:layers:mentoring:AnswerApproval",
  "body": "https://ikigai-rs.dev/ns#body",
  "brief": "https://ikigai-rs.dev/ns#brief",
  "inReplyTo": { "@id": "https://ikigai-rs.dev/ns#inReplyTo", "@type": "@id" },
  "receipt": { "@id": "urn:layers:mentoring:receipt", "@type": "@id" },
  "question": { "@id": "urn:layers:mentoring:question", "@type": "@id" },
  "artifact": { "@id": "urn:layers:mentoring:artifact", "@type": "@id" },
  "answer": { "@id": "urn:layers:mentoring:answer", "@type": "@id" }
} }"#;

const REPLY_CHECK: &str = "
[[reference]]
path = \"https://ikigai-rs.dev/ns#inReplyTo\"
layer = \"question\"
class = \"urn:layers:mentoring:Question\"
";

const RESULT_CHECK: &str = "
[result]
layer = \"question\"
class = \"urn:layers:mentoring:Answer\"
";

const APPROVAL_CHECK: &str = "
[[reference]]
path = \"urn:layers:mentoring:answer\"
layer = \"question\"
class = \"urn:layers:mentoring:Answer\"
independent = true
";

/// The three mentoring layers beside the fixture's, with the checks on or off.
fn mentoring(checks: bool) -> (Fixture, Arc<ManualClock>) {
    let mut f = fixture();
    let on = |check: &'static str| if checks { check } else { "" };
    f.config
        .write("layers/question.shapes.ttl", &question_shapes());
    f.config
        .write("layers/review-offer.shapes.ttl", &offer_shapes());
    f.config
        .write("layers/answer-approval.shapes.ttl", &approval_shapes());
    for name in ["question", "review-offer", "answer-approval"] {
        f.config
            .write(&format!("layers/{name}.context.jsonld"), MENTORING_CONTEXT);
    }
    f.config.write(
        "layers/question.toml",
        &format!(
            "model = \"channel\"\nkind = \"talk\"\nshapes = \"question.shapes.ttl\"\n\
             context = \"question.context.jsonld\"\n{}",
            on(REPLY_CHECK)
        ),
    );
    f.config.write(
        "layers/review-offer.toml",
        &format!(
            "model = \"tuplespace\"\nkind = \"task\"\nshapes = \"review-offer.shapes.ttl\"\n\
             context = \"review-offer.context.jsonld\"\nlease_max = \"1h\"\n{}",
            on(RESULT_CHECK)
        ),
    );
    f.config.write(
        "layers/answer-approval.toml",
        &format!(
            "model = \"channel\"\nshapes = \"answer-approval.shapes.ttl\"\n\
             context = \"answer-approval.context.jsonld\"\nabout = \"{M}answer\"\n{}",
            on(APPROVAL_CHECK)
        ),
    );
    f.reopen();
    let clock = Arc::new(ManualClock(AtomicU64::new(START)));
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
    (f, clock)
}

fn at(f: &mut Fixture, clock: &Arc<ManualClock>, minutes: u64) {
    clock.0.store(START + minutes * MINUTE, Ordering::SeqCst);
    f.kernel = kernel_over(&f.host).with_clock(clock.clone());
}

/// The grants of the design's table, for one role, by layer and verbs.
fn grants(principal: &str, held: &[(&str, &[&str])]) -> Capability {
    let scopes: Vec<String> = held
        .iter()
        .flat_map(|(layer, verbs)| {
            verbs
                .iter()
                .map(move |v| format!("urn:cap:layer:{v}:{layer}"))
        })
        .collect();
    let scopes: Vec<&str> = scopes.iter().map(String::as_str).collect();
    cap(principal, &scopes)
}

fn participant_of(who: &str) -> Capability {
    grants(
        who,
        &[
            ("question", &["read", "feed", "say"]),
            ("review-offer", &["read", "feed", "out"]),
            ("answer-approval", &["read", "feed"]),
        ],
    )
}

fn mentor_of(who: &str) -> Capability {
    grants(
        who,
        &[
            ("question", &["read", "feed", "say"]),
            ("review-offer", &["read", "feed", "take"]),
            ("answer-approval", &["read", "feed"]),
        ],
    )
}

fn approver_of(who: &str) -> Capability {
    grants(
        who,
        &[
            ("question", &["read", "feed", "say"]),
            ("review-offer", &["read"]),
            ("answer-approval", &["read", "feed", "say"]),
        ],
    )
}

fn asker() -> String {
    participant("bobby")
}
fn mentor() -> String {
    participant("chris")
}
fn approver() -> String {
    participant("brian")
}

fn say_as(f: &Fixture, layer: &str, who: &Capability, content: &str) -> Result<String, Error> {
    issue(
        &f.kernel,
        Verb::Sink,
        &format!("urn:layer:{layer}:say"),
        &[("content", content)],
        who,
    )
    .map(|r| text(&r))
}

fn question(body: &str) -> String {
    format!("@prefix m: <{M}> .\n@prefix ik: <{IK}> .\n<urn:layer:new> a m:Question ; ik:body \"{body}\" .\n")
}

fn answer(to: &str, receipt: Option<&str>) -> String {
    let receipt = receipt
        .map(|r| format!(" ; m:receipt <{r}>"))
        .unwrap_or_default();
    format!(
        "@prefix m: <{M}> .\n@prefix ik: <{IK}> .\n\
         <urn:layer:new> a m:Answer ; ik:body \"an answer\" ; ik:inReplyTo <{to}>{receipt} .\n"
    )
}

fn approval(of: &str) -> String {
    format!("@prefix m: <{M}> .\n<urn:layer:new> a m:AnswerApproval ; m:answer <{of}> .\n")
}

const OFFER: &str = "urn:layer:review-offer:offer:01jz0000000000000000000000";

fn review_offer(question: &str) -> String {
    format!(
        "@prefix m: <{M}> .\n@prefix ik: <{IK}> .\n\
         <{OFFER}> a m:ReviewOffer ; m:question <{question}> ;\n\
         m:artifact <https://github.com/ikigai-rs/ikigai-layers/commit/2dc4277> ;\n\
         ik:brief \"Review this revision.\" .\n"
    )
}

/// A missing entry of the question channel: a well-formed IRI no `say` minted.
const MISSING: &str = "urn:layer:question:entry:01jz000000000000000000z";

fn refused_content(result: Result<String, Error>) -> String {
    match result {
        Err(Error::InvalidArgument { name, detail }) if name == "content" => detail,
        other => panic!("expected a refused content, got {other:?}"),
    }
}

#[test]
fn without_the_checks_a_dangling_reply_is_admitted() {
    // The reproduction (ledger #4274): the write path validates only the submitted graph, so
    // a reply naming no entry at all, or an entry of the wrong class, is stored.
    let (f, _) = mentoring(false);
    assert!(say_as(
        &f,
        "question",
        &mentor_of(&mentor()),
        &answer(MISSING, None)
    )
    .is_ok());
    let q = say_as(&f, "question", &participant_of(&asker()), &question("why?")).unwrap();
    let a = say_as(&f, "question", &mentor_of(&mentor()), &answer(&q, None)).unwrap();
    assert!(say_as(&f, "question", &mentor_of(&mentor()), &answer(&a, None)).is_ok());
    // And an approver may approve their own answer.
    let own = say_as(&f, "question", &approver_of(&approver()), &answer(&q, None)).unwrap();
    assert!(say_as(
        &f,
        "answer-approval",
        &approver_of(&approver()),
        &approval(&own)
    )
    .is_ok());
}

#[test]
fn a_reply_must_name_an_existing_question() {
    let (f, _) = mentoring(true);
    let mentor = mentor_of(&mentor());
    // Missing.
    let detail = refused_content(say_as(&f, "question", &mentor, &answer(MISSING, None)));
    assert!(
        detail.contains(MISSING) && detail.contains("no entry"),
        "{detail}"
    );
    let q = say_as(&f, "question", &participant_of(&asker()), &question("why?")).unwrap();
    let a = say_as(&f, "question", &mentor, &answer(&q, None)).unwrap();
    // Wrong class: an answer is no question.
    let detail = refused_content(say_as(&f, "question", &mentor, &answer(&a, None)));
    assert!(
        detail.contains("not a <urn:layers:mentoring:Question>"),
        "{detail}"
    );
    // Nothing refused was stored: the question and the one answer.
    let feed = source(
        &f.kernel,
        "urn:layer:question:feed",
        &[("after", "0")],
        &mentor,
    );
    assert_eq!(feed.lines().count(), 2, "{feed}");
}

#[test]
fn a_reference_check_reads_under_the_writers_grants() {
    let (f, _) = mentoring(true);
    let q = say_as(&f, "question", &participant_of(&asker()), &question("why?")).unwrap();
    let a = say_as(&f, "question", &mentor_of(&mentor()), &answer(&q, None)).unwrap();
    // An approver who cannot read the question channel is refused for that, the same for an
    // answer that exists and one that does not: the check reveals nothing it could not read.
    let blind = grants(
        &approver(),
        &[("answer-approval", &["read", "feed", "say"])],
    );
    let refusal = |target: &str| match say_as(&f, "answer-approval", &blind, &approval(target)) {
        Err(Error::Denied(d)) => d.to_string(),
        other => panic!("expected a denial, got {other:?}"),
    };
    let existing = refusal(&a);
    let missing = refusal(MISSING);
    assert_eq!(existing, missing);
    assert!(
        existing.contains("urn:cap:layer:read:question"),
        "{existing}"
    );
}

#[test]
fn an_approval_must_name_another_participants_answer() {
    let (f, _) = mentoring(true);
    let approver = approver_of(&approver());
    let q = say_as(&f, "question", &participant_of(&asker()), &question("why?")).unwrap();
    // Missing, and wrong class (a question is no answer).
    let detail = refused_content(say_as(&f, "answer-approval", &approver, &approval(MISSING)));
    assert!(detail.contains("no entry"), "{detail}");
    let detail = refused_content(say_as(&f, "answer-approval", &approver, &approval(&q)));
    assert!(
        detail.contains("not a <urn:layers:mentoring:Answer>"),
        "{detail}"
    );
    // Their own answer.
    let own = say_as(&f, "question", &approver, &answer(&q, None)).unwrap();
    match say_as(&f, "answer-approval", &approver, &approval(&own)) {
        Err(Error::Denied(d)) => assert!(d.to_string().contains("another participant"), "{d}"),
        other => panic!("expected a denial, got {other:?}"),
    }
    // Someone else's answer.
    let a = say_as(&f, "question", &mentor_of(&mentor()), &answer(&q, None)).unwrap();
    let approved = say_as(&f, "answer-approval", &approver, &approval(&a)).unwrap();
    assert!(
        approved.starts_with("urn:layer:answer-approval:entry:"),
        "{approved}"
    );
}

/// The receipt the mentor's leased take of the offer committed.
fn take_review(f: &Fixture) -> String {
    issue(
        &f.kernel,
        Verb::Delete,
        "urn:layer:review-offer:tuples",
        &[("tuple", OFFER), ("lease", "30m")],
        &mentor_of(&mentor()),
    )
    .unwrap();
    let listed: serde_json::Value = serde_json::from_str(&source(
        &f.kernel,
        "urn:layer:review-offer:tuples",
        &[("state", "taken"), ("as", "application/json")],
        &mentor_of(&mentor()),
    ))
    .unwrap();
    listed["receipts"][0]["receipt"]
        .as_str()
        .unwrap()
        .to_string()
}

fn settle_done(
    f: &Fixture,
    who: &Capability,
    receipt: &str,
    result: &str,
) -> Result<String, Error> {
    issue(
        &f.kernel,
        Verb::Sink,
        receipt,
        &[("settle", "done"), ("result", result)],
        who,
    )
    .map(|r| text(&r))
}

fn refused_result(result: Result<String, Error>) -> String {
    match result {
        Err(Error::InvalidArgument { name, detail }) if name == "result" => detail,
        other => panic!("expected a refused result, got {other:?}"),
    }
}

#[test]
fn a_receipts_result_must_be_an_answer_its_taker_wrote() {
    let (f, _) = mentoring(true);
    let q = say_as(&f, "question", &participant_of(&asker()), &question("why?")).unwrap();
    issue(
        &f.kernel,
        Verb::Sink,
        "urn:layer:review-offer:tuples",
        &[("content", &review_offer(&q))],
        &participant_of(&asker()),
    )
    .unwrap();
    let receipt = take_review(&f);
    let mentor = mentor_of(&mentor());
    // Missing, not an entry IRI at all, and the wrong class.
    assert!(refused_result(settle_done(&f, &mentor, &receipt, MISSING)).contains("no entry"));
    let not_an_entry = "https://github.com/ikigai-rs/ikigai-layers/pull/1";
    assert!(
        refused_result(settle_done(&f, &mentor, &receipt, not_an_entry))
            .contains("no entry IRI of layer `question`")
    );
    assert!(refused_result(settle_done(&f, &mentor, &receipt, &q))
        .contains("not a <urn:layers:mentoring:Answer>"));
    // An answer someone other than the taker wrote: a mismatched taker.
    let theirs = say_as(
        &f,
        "question",
        &approver_of(&approver()),
        &answer(&q, Some(&receipt)),
    )
    .unwrap();
    let detail = refused_result(settle_done(&f, &mentor, &receipt, &theirs));
    assert!(detail.contains("not by this receipt's taker"), "{detail}");
    // With the check on, a done settle names its result.
    match issue(
        &f.kernel,
        Verb::Sink,
        &receipt,
        &[("settle", "done")],
        &mentor,
    ) {
        Err(Error::MissingArgument(name)) => assert_eq!(name, "result"),
        other => panic!("expected a missing result, got {other:?}"),
    }
    // The taker's own answer settles it, once.
    let own = say_as(&f, "question", &mentor, &answer(&q, Some(&receipt))).unwrap();
    assert_eq!(settle_done(&f, &mentor, &receipt, &own).unwrap(), receipt);
    let listed = source(
        &f.kernel,
        "urn:layer:review-offer:tuples",
        &[("state", "taken")],
        &mentor,
    );
    assert!(listed.contains(&format!("result={own}")), "{listed}");
}

#[test]
fn a_late_settlement_is_refused_even_naming_a_valid_answer() {
    let (mut f, clock) = mentoring(true);
    let q = say_as(&f, "question", &participant_of(&asker()), &question("why?")).unwrap();
    issue(
        &f.kernel,
        Verb::Sink,
        "urn:layer:review-offer:tuples",
        &[("content", &review_offer(&q))],
        &participant_of(&asker()),
    )
    .unwrap();
    let receipt = take_review(&f);
    let mentor = mentor_of(&mentor());
    let own = say_as(&f, "question", &mentor, &answer(&q, Some(&receipt))).unwrap();
    // Past the 30m lease.
    at(&mut f, &clock, 31);
    match settle_done(&f, &mentor, &receipt, &own) {
        Err(Error::Conflict(d)) => assert!(d.contains("lapsed"), "{d}"),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn a_check_naming_no_channel_is_refused_at_open() {
    let (f, _) = mentoring(true);
    for (layer, check, says) in [
        (
            "question",
            "[[reference]]\npath = \"urn:x\"\nlayer = \"nowhere\"\nclass = \"urn:x\"\n",
            "does not define",
        ),
        (
            "question",
            "[[reference]]\npath = \"urn:x\"\nlayer = \"review-offer\"\nclass = \"urn:x\"\n",
            "only a channel holds",
        ),
        (
            "question",
            "[[reference]]\npath = \"urn:x\"\nlayer = \"question\"\nclass = \"urn:x\"\n",
            "do not target",
        ),
        ("question", RESULT_CHECK, "tuplespace with a lease_max"),
    ] {
        let path = f.config.path().join(format!("layers/{layer}.toml"));
        let before = std::fs::read_to_string(&path).unwrap();
        f.config
            .write(&format!("layers/{layer}.toml"), &format!("{before}{check}"));
        let e = ikigai_layers::Host::open(
            f.config.path(),
            f.config.path().join("other-state"),
            &key(f.config.path()),
        )
        .err()
        .unwrap_or_else(|| panic!("{check} opened"));
        assert!(e.to_string().contains(says), "{check}: {e}");
        f.config.write(&format!("layers/{layer}.toml"), &before);
    }
}
