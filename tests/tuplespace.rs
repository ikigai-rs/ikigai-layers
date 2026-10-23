//! The tuplespace model (ledger #1187): offers and addressed reports, an atomic take that
//! is a claim, receipts naming the taker, subject uniqueness and idempotent replay.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::sync::{Arc, Barrier};

use common::*;
use ikigai_core::{Capability, Error, Kernel, Verb};

fn plasma() -> String {
    gonk()
}

fn rd(kernel: &Kernel, args: &[(&str, &str)]) -> String {
    source(kernel, TUPLES, args, &tasker(&participant("bobby")))
}

fn feed(kernel: &Kernel) -> Vec<String> {
    source(
        kernel,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &tasker(&participant("bobby")),
    )
    .lines()
    .map(String::from)
    .collect()
}

fn kinds(kernel: &Kernel) -> Vec<String> {
    feed(kernel)
        .iter()
        .map(|l| l.split(' ').nth(2).unwrap().to_string())
        .collect()
}

#[test]
fn an_offer_is_named_by_its_dropper_and_stamped_by_the_host() {
    let f = fixture();
    let subject = out(&f.kernel, &plasma(), &offer("a")).unwrap();
    assert_eq!(subject, offer_iri("a"));

    let list = rd(&f.kernel, &[]);
    let line: Vec<&str> = list.trim().split(' ').collect();
    assert_eq!(line[0], "1");
    assert_eq!(line[2], offer_iri("a"));
    assert_eq!(line[3], plasma());

    let tuple = rd(&f.kernel, &[("tuple", &offer_iri("a"))]);
    let triples = triples(&tuple);
    let has = |p: &str, o: &str| {
        triples
            .iter()
            .any(|t| t.predicate.as_str() == p && t.object.to_string().contains(o))
    };
    assert!(has("http://www.w3.org/ns/prov#wasAttributedTo", &plasma()));
    assert!(has("http://purl.org/dc/terms/created", "2026-10-10T"));
    assert!(has(&format!("{IK}seq"), "\"1\""));
    assert!(triples
        .iter()
        .all(|t| t.subject.to_string() == format!("<{}>", offer_iri("a"))));
    // Never ik:layer: the layer a tuple belongs to is the segment of its own IRI.
    assert!(!tuple.contains("ik:layer ") && !tuple.contains("ns#layer>"));

    assert_eq!(kinds(&f.kernel), ["out"]);
    assert!(feed(&f.kernel)[0].ends_with(&format!("out {} {}", offer_iri("a"), plasma())));
}

#[test]
fn a_tuple_is_one_subject_the_dropper_names_under_the_layer() {
    let f = fixture();
    let prefix = format!("@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n");
    let body = "a ik:Offer ; dcterms:title \"t\" ; ik:brief \"b\" .";
    for (content, says) in [
        (format!("{prefix}<urn:layer:talk:offer:x> {body}"), "is named under"),
        (format!("{prefix}<urn:example:offer> {body}"), "is named under"),
        (format!("{prefix}<urn:layer:tasks-bobby:feed> {body}"), "own resources"),
        (format!("{prefix}<urn:layer:tasks-bobby:receipt:x> {body}"), "own resources"),
        (format!("{prefix}<urn:layer:tasks-bobby:> {body}"), "own resources"),
        (format!("{prefix}[] {body}"), "blank-node subject"),
        (
            format!("{prefix}<urn:layer:tasks-bobby:offer:x> {body}\n<urn:layer:tasks-bobby:offer:y> {body}"),
            "more than one subject",
        ),
        (
            format!("{prefix}<urn:layer:tasks-bobby:offer:x> {body}\n<urn:layer:tasks-bobby:offer:x> ik:repository [] ."),
            "blank node",
        ),
        (
            format!("{prefix}<urn:layer:tasks-bobby:offer:x> {body}\n<urn:layer:tasks-bobby:offer:x> ik:seq 4 ."),
            "stamped by the host",
        ),
        (
            format!("{prefix}<urn:layer:tasks-bobby:offer:x> {body}\n<urn:layer:tasks-bobby:offer:x> <http://www.w3.org/ns/prov#wasAttributedTo> <urn:collab:participant:brian> ."),
            "stamped by the host",
        ),
        (
            format!("{prefix}<urn:layer:tasks-bobby:offer:x> a ik:Message ; ik:body \"hi\" ."),
            "admits only",
        ),
    ] {
        match out(&f.kernel, &plasma(), &content) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, "content");
                assert!(detail.contains(says), "{content}\n=> {detail}");
            }
            other => panic!("{content}\n=> {other:?}"),
        }
    }
    assert!(feed(&f.kernel).is_empty(), "nothing refused is logged");
}

#[test]
fn a_non_conforming_tuple_is_refused() {
    let f = fixture();
    let prefix = format!("@prefix ik: <{IK}> .\n@prefix dcterms: <http://purl.org/dc/terms/> .\n");
    for content in [
        // No brief.
        format!("{prefix}<urn:layer:tasks-bobby:offer:x> a ik:Offer ; dcterms:title \"t\" ."),
        // A predicate the closed shape does not name.
        format!(
            "{prefix}<urn:layer:tasks-bobby:offer:x> a ik:Offer ; dcterms:title \"t\" ; ik:brief \"b\" ; ik:note \"n\" ."
        ),
        // A hand-back with no evidence (the second shape on ik:Handback).
        format!(
            "{prefix}<urn:layer:tasks-bobby:report:x> a ik:Handback ; ik:offer <{}> ; ik:addressedTo <{}> .",
            offer_iri("a"),
            plasma()
        ),
    ] {
        // The hand-back needs a taken offer to get as far as the shapes.
        let f2 = fixture();
        out(&f2.kernel, &plasma(), &offer("a")).unwrap();
        take(&f2.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
        match out(&f2.kernel, &bobby_gonk(), &content) {
            Err(Error::InvalidArgument { detail, .. }) => {
                assert!(detail.contains("does not conform"), "{content}\n=> {detail}")
            }
            other => panic!("{content}\n=> {other:?}"),
        }
        assert_eq!(kinds(&f2.kernel), ["out", "taken"], "nothing refused is logged");
    }
    assert!(rd(&f.kernel, &[]).is_empty());
}

#[test]
fn a_reused_subject_is_refused() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    // Another tuple under the same name: refused, by anyone.
    let other = offer("a").replace("Do a.", "Do something else.");
    for who in [plasma(), bobby_gonk()] {
        let refused = out(&f.kernel, &who, &other);
        assert!(
            matches!(refused, Err(Error::Conflict(ref m)) if m.contains("already names")),
            "{refused:?}"
        );
    }
    // The same content from someone else is not a replay of the first drop.
    let refused = out(&f.kernel, &bobby_gonk(), &offer("a"));
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    // Once taken, the name stays used.
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let refused = out(&f.kernel, &plasma(), &other);
    assert!(matches!(refused, Err(Error::Conflict(_))), "{refused:?}");
    assert_eq!(kinds(&f.kernel), ["out", "taken"]);
}

/// The same hand-back seen twice is one record: a retry after a lost reply is answered as
/// the drop it repeats, with no new event, whether the first is still pending or taken.
#[test]
fn the_same_drop_twice_is_one_record() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let handback = report("Handback", "h1", &offer_iri("a"), &plasma());
    assert_eq!(
        out(&f.kernel, &bobby_gonk(), &handback).unwrap(),
        report_iri("h1")
    );
    assert_eq!(
        out(&f.kernel, &bobby_gonk(), &handback).unwrap(),
        report_iri("h1")
    );
    assert_eq!(kinds(&f.kernel), ["out", "taken", "out"]);
    // The offerer takes it once; the replay after that is still the one record.
    take(&f.kernel, &plasma(), &report_iri("h1")).unwrap();
    assert_eq!(
        out(&f.kernel, &bobby_gonk(), &handback).unwrap(),
        report_iri("h1")
    );
    assert_eq!(kinds(&f.kernel), ["out", "taken", "out", "taken"]);
    // And across a restart (the replay check is rebuilt from what is stored).
    let mut f = f;
    f.reopen();
    assert_eq!(
        out(&f.kernel, &bobby_gonk(), &handback).unwrap(),
        report_iri("h1")
    );
    assert_eq!(kinds(&f.kernel), ["out", "taken", "out", "taken"]);
    // New evidence is a new report, with a new name.
    let again = report("Handback", "h2", &offer_iri("a"), &plasma());
    assert_eq!(
        out(&f.kernel, &bobby_gonk(), &again).unwrap(),
        report_iri("h2")
    );
}

/// Two racers, one winner, a receipt naming it; the loser gets a typed Conflict naming the
/// winner. Many rounds, both takers released together each time.
#[test]
fn two_racers_one_winner_and_a_receipt_naming_it() {
    let f = fixture();
    const ROUNDS: usize = 24;
    for i in 0..ROUNDS {
        out(&f.kernel, &plasma(), &offer(&format!("r{i}"))).unwrap();
    }
    let kernel = Arc::new(f.kernel);
    let racers = [bobby_gonk(), participant("bobby")];
    let mut winners = Vec::new();
    for i in 0..ROUNDS {
        let subject = offer_iri(&format!("r{i}"));
        let barrier = Arc::new(Barrier::new(racers.len()));
        let results: Vec<(String, Result<String, Error>)> = std::thread::scope(|scope| {
            let handles: Vec<_> = racers
                .iter()
                .map(|who| {
                    let (kernel, barrier, subject) = (&kernel, Arc::clone(&barrier), &subject);
                    scope.spawn(move || {
                        barrier.wait();
                        (who.clone(), take(kernel, who, subject))
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let won: Vec<&String> = results
            .iter()
            .filter(|(_, r)| r.is_ok())
            .map(|(w, _)| w)
            .collect();
        assert_eq!(won.len(), 1, "round {i}: {results:?}");
        let winner = won[0].clone();
        for (who, result) in &results {
            if *who != winner {
                match result {
                    Err(Error::Conflict(m)) => {
                        assert!(m.contains(&format!("taken by <{winner}>")), "{m}")
                    }
                    other => panic!("round {i}: the loser got {other:?}"),
                }
            }
        }
        let receipt = source(
            &kernel,
            TUPLES,
            &[("state", "taken"), ("tuple", &subject)],
            &tasker(&participant("bobby")),
        );
        assert!(
            receipt.contains(&format!("prov:wasAssociatedWith <{winner}>")),
            "{receipt}"
        );
        winners.push(winner);
    }
    let all = feed(&kernel);
    assert_eq!(all.len(), 2 * ROUNDS);
    for (n, line) in all.iter().enumerate() {
        assert!(line.starts_with(&format!("{} ", n + 1)), "gapless: {line}");
    }
    let taken: Vec<&String> = all.iter().filter(|l| l.contains(" taken ")).collect();
    assert_eq!(taken.len(), ROUNDS);
    assert!(
        rd(&kernel, &[]).is_empty(),
        "every offer taken exactly once"
    );
    let listed = rd(&kernel, &[("state", "taken")]);
    assert_eq!(listed.lines().count(), ROUNDS);
}

/// The take-vs-revoke race (design section 10): the offerer revokes by taking its own
/// offer while the collaborator takes it. Exactly one outcome per offer, and the receipt
/// says which.
#[test]
fn a_take_racing_a_revocation_has_one_outcome() {
    let f = fixture();
    const ROUNDS: usize = 24;
    for i in 0..ROUNDS {
        out(&f.kernel, &plasma(), &offer(&format!("v{i}"))).unwrap();
    }
    let kernel = Arc::new(f.kernel);
    let (mut revoked, mut taken) = (0, 0);
    for i in 0..ROUNDS {
        let subject = offer_iri(&format!("v{i}"));
        let barrier = Arc::new(Barrier::new(2));
        let (revoke, claim) = std::thread::scope(|scope| {
            let revoke = {
                let (kernel, barrier, subject) = (&kernel, Arc::clone(&barrier), &subject);
                scope.spawn(move || {
                    barrier.wait();
                    take(kernel, &plasma(), subject)
                })
            };
            let claim = {
                let (kernel, barrier, subject) = (&kernel, Arc::clone(&barrier), &subject);
                scope.spawn(move || {
                    barrier.wait();
                    take(kernel, &bobby_gonk(), subject)
                })
            };
            (revoke.join().unwrap(), claim.join().unwrap())
        });
        let receipt = source(
            &kernel,
            TUPLES,
            &[("state", "taken"), ("tuple", &subject)],
            &tasker(&participant("bobby")),
        );
        match (&revoke, &claim) {
            (Ok(_), Err(Error::Conflict(m))) => {
                revoked += 1;
                assert!(m.contains(&plasma()), "{m}");
                assert!(receipt.contains(&format!("prov:wasAssociatedWith <{}>", plasma())));
            }
            (Err(Error::Conflict(m)), Ok(_)) => {
                taken += 1;
                assert!(m.contains(&bobby_gonk()), "{m}");
                assert!(receipt.contains(&format!("prov:wasAssociatedWith <{}>", bobby_gonk())));
            }
            other => panic!("round {i}: not exactly one outcome: {other:?}"),
        }
    }
    assert_eq!(revoked + taken, ROUNDS);
    assert!(rd(&kernel, &[]).is_empty());
}

#[test]
fn a_report_addressed_to_a_cannot_be_taken_by_b() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    out(
        &f.kernel,
        &bobby_gonk(),
        &report("Progress", "p1", &offer_iri("a"), &plasma()),
    )
    .unwrap();
    for b in [participant("bobby"), bobby_gonk()] {
        let refused = take(&f.kernel, &b, &report_iri("p1"));
        assert!(
            matches!(refused, Err(Error::Denied(ref m)) if m.contains("addressed to")),
            "{b}: {refused:?}"
        );
    }
    // Still pending, and the addressee takes it.
    assert!(rd(&f.kernel, &[]).contains(&report_iri("p1")));
    let got = take(&f.kernel, &plasma(), &report_iri("p1")).unwrap();
    assert!(got.contains("report p1"), "{got}");
    // An address nobody could take is refused at out.
    let stranger = report(
        "Progress",
        "p2",
        &offer_iri("a"),
        "urn:collab:participant:mallory",
    );
    let refused = out(&f.kernel, &bobby_gonk(), &stranger);
    assert!(
        matches!(refused, Err(Error::InvalidArgument { ref detail, .. }) if detail.contains("no participant")),
        "{refused:?}"
    );
}

/// An unaddressed layer takes no notice of ik:addressedTo: the option is the operator's.
#[test]
fn addressing_binds_only_in_an_addressed_layer() {
    let f = fixture();
    let def = f.config.path().join("layers/tasks-bobby.toml");
    let text = std::fs::read_to_string(&def).unwrap();
    std::fs::write(&def, text.replace("addressed = true\n", "")).unwrap();
    let mut f = f;
    f.reopen();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    out(
        &f.kernel,
        &bobby_gonk(),
        &report("Progress", "p1", &offer_iri("a"), &plasma()),
    )
    .unwrap();
    take(&f.kernel, &participant("bobby"), &report_iri("p1")).unwrap();
}

#[test]
fn only_an_offers_taker_reports_on_it() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    // Pending: nothing to report on yet.
    let refused = out(
        &f.kernel,
        &bobby_gonk(),
        &report("Progress", "p0", &offer_iri("a"), &plasma()),
    );
    assert!(
        matches!(refused, Err(Error::Conflict(ref m)) if m.contains("still pending")),
        "{refused:?}"
    );
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    // Not the taker: a forged hand-back is refused.
    let forged = out(
        &f.kernel,
        &participant("bobby"),
        &report("Handback", "h0", &offer_iri("a"), &plasma()),
    );
    assert!(
        matches!(forged, Err(Error::Denied(ref m)) if m.contains("only its taker")),
        "{forged:?}"
    );
    // No such offer.
    let unknown = out(
        &f.kernel,
        &bobby_gonk(),
        &report("Release", "r0", &offer_iri("nope"), &plasma()),
    );
    assert!(
        matches!(unknown, Err(Error::InvalidArgument { .. })),
        "{unknown:?}"
    );
    // Addressed to anyone but the offerer, nobody who should could take it.
    let misaddressed = out(
        &f.kernel,
        &bobby_gonk(),
        &report("Release", "r0", &offer_iri("a"), &participant("brian")),
    );
    assert!(
        matches!(misaddressed, Err(Error::InvalidArgument { ref detail, .. }) if detail.contains("addressed to its offerer")),
        "{misaddressed:?}"
    );
    // The taker reports, to the offerer.
    out(
        &f.kernel,
        &bobby_gonk(),
        &report("Release", "r1", &offer_iri("a"), &plasma()),
    )
    .unwrap();
}

/// A taker that lost its reply finds its own take: the receipt is written before the reply.
#[test]
fn a_lost_reply_taker_finds_its_own_take() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let _lost = take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();

    // Retrying the take says who has it.
    let again = take(&f.kernel, &bobby_gonk(), &offer_iri("a"));
    assert!(
        matches!(again, Err(Error::Conflict(ref m)) if m.contains(&format!("taken by <{}>", bobby_gonk()))),
        "{again:?}"
    );
    // The receipt names the taker, the time, and holds the tuple as taken.
    let receipt = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    );
    assert!(receipt.contains("prov:Activity"), "{receipt}");
    assert!(
        receipt.contains(&format!("prov:wasAssociatedWith <{}>", bobby_gonk())),
        "{receipt}"
    );
    assert!(
        receipt.contains(&format!("prov:used <{}>", offer_iri("a"))),
        "{receipt}"
    );
    assert!(receipt.contains("Do a."), "the tuple itself: {receipt}");
    assert!(!receipt.contains("ns#layer>") && !receipt.contains("ik:layer "));
    // And by its own IRI, which never changes.
    let iri = triples(&receipt)
        .iter()
        .map(|t| t.subject.to_string())
        .find(|s| s.contains(":receipt:"))
        .unwrap();
    let iri = iri.trim_matches(|c| c == '<' || c == '>');
    let by_iri = source(&f.kernel, iri, &[], &tasker(&participant("bobby")));
    assert_eq!(by_iri, receipt);
    // The listing too.
    let listed = rd(&f.kernel, &[("state", "taken")]);
    assert!(
        listed
            .trim()
            .ends_with(&format!("{} {}", offer_iri("a"), bobby_gonk())),
        "{listed}"
    );
    // The pending read says where it went.
    let pending = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("tuple", &offer_iri("a"))],
        &tasker(&participant("bobby")),
    );
    assert!(
        matches!(pending, Err(Error::NotFound(ref m)) if m.contains("state=taken")),
        "{pending:?}"
    );
}

#[test]
fn take_and_out_are_attributed() {
    let f = fixture();
    let root = Capability::root();
    for (verb, args) in [
        (Verb::Sink, vec![("content", offer("a"))]),
        (Verb::Delete, vec![("tuple", offer_iri("a"))]),
    ] {
        let args: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let refused = issue(&f.kernel, verb, TUPLES, &args, &root);
        assert!(
            matches!(refused, Err(Error::Denied(ref m)) if m.contains("needs a principal")),
            "{refused:?}"
        );
        let stranger = cap(
            "urn:collab:participant:mallory",
            &[
                "urn:cap:layer:out:tasks-bobby",
                "urn:cap:layer:take:tasks-bobby",
            ],
        );
        let refused = issue(&f.kernel, verb, TUPLES, &args, &stranger);
        assert!(
            matches!(refused, Err(Error::Denied(ref m)) if m.contains("not a participant")),
            "{refused:?}"
        );
    }
    // A take names its tuple; no pop.
    let refused = issue(&f.kernel, Verb::Delete, TUPLES, &[], &tasker(&bobby_gonk()));
    assert!(
        matches!(refused, Err(Error::MissingArgument(_))),
        "{refused:?}"
    );
    // The piped form names it too.
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let piped = issue(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("content", &format!("{}\n", offer_iri("a")))],
        &tasker(&bobby_gonk()),
    );
    assert!(piped.is_ok(), "{piped:?}");
    let missing = take(&f.kernel, &bobby_gonk(), &offer_iri("nope"));
    assert!(matches!(missing, Err(Error::NotFound(_))), "{missing:?}");
}

/// Each verb needs its own exact capability: read does not out, out does not take.
#[test]
fn each_verb_needs_its_own_exact_capability() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let reader = cap(&participant("bobby"), &["urn:cap:layer:read:tasks-bobby"]);
    assert!(issue(&f.kernel, Verb::Source, TUPLES, &[], &reader).is_ok());
    let refused = issue(
        &f.kernel,
        Verb::Sink,
        TUPLES,
        &[("content", &offer("b"))],
        &reader,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    let dropper = cap(&participant("bobby"), &["urn:cap:layer:out:tasks-bobby"]);
    let refused = issue(
        &f.kernel,
        Verb::Delete,
        TUPLES,
        &[("tuple", &offer_iri("a"))],
        &dropper,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
    // Another layer's token, or the family's literal, is not this layer's.
    for wrong in [
        "urn:cap:layer:take:tasks",
        "urn:cap:layer:take:*",
        "urn:cap:space:take",
    ] {
        let held = cap(&bobby_gonk(), &[wrong]);
        let refused = issue(
            &f.kernel,
            Verb::Delete,
            TUPLES,
            &[("tuple", &offer_iri("a"))],
            &held,
        );
        assert!(
            matches!(refused, Err(Error::Denied(_))),
            "{wrong}: {refused:?}"
        );
    }
    let refused = issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:tasks-bobby:feed",
        &[("after", "0")],
        &reader,
    );
    assert!(matches!(refused, Err(Error::Denied(_))), "{refused:?}");
}

#[test]
fn rd_reads_by_template_and_pages_by_sequence() {
    let f = matching_fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let other = offer("b").replace(
        "https://github.com/ikigai-rs/ikigai-layers",
        "https://github.com/ikigai-rs/ikigai-core",
    );
    out(&f.kernel, &plasma(), &other).unwrap();
    out(&f.kernel, &plasma(), &offer("c")).unwrap();
    let ask = format!("ASK {{ ?o <{IK}repository> <https://github.com/ikigai-rs/ikigai-core> }}");
    let hits = rd(&f.kernel, &[("match", &ask)]);
    assert_eq!(hits.lines().count(), 1, "{hits}");
    assert!(hits.contains(&offer_iri("b")));
    let page = rd(&f.kernel, &[("after", "1"), ("limit", "1")]);
    assert_eq!(page.lines().count(), 1);
    assert!(page.starts_with("2 "), "{page}");
    let as_turtle = rd(&f.kernel, &[("as", "text/turtle")]);
    assert_eq!(
        triples(&as_turtle)
            .iter()
            .filter(|t| t.predicate.as_str().ends_with("#seq"))
            .count(),
        3
    );
    let refused = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("match", "SELECT * WHERE { ?s ?p ?o }")],
        &tasker(&participant("bobby")),
    );
    assert!(
        matches!(refused, Err(Error::InvalidArgument { .. })),
        "{refused:?}"
    );
    let refused = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("state", "done")],
        &tasker(&participant("bobby")),
    );
    assert!(
        matches!(refused, Err(Error::InvalidArgument { .. })),
        "{refused:?}"
    );
}

/// rd is cacheable, and out and take cut it: a read after each sees the change.
#[test]
fn reads_follow_every_out_and_take() {
    let f = fixture();
    assert_eq!(rd(&f.kernel, &[]), "");
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    assert_eq!(rd(&f.kernel, &[]).lines().count(), 1);
    assert_eq!(rd(&f.kernel, &[("state", "taken")]), "");
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    assert_eq!(rd(&f.kernel, &[]), "");
    assert_eq!(rd(&f.kernel, &[("state", "taken")]).lines().count(), 1);
}

/// Something other than this host took a tuple from the space (the host's own takers are
/// serialized; this is intray's rename losing): a Conflict, never a receipt invented for
/// nobody, and the name stays used.
#[test]
fn a_tuple_taken_from_under_the_host_is_a_conflict() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let intray = Kernel::new(Arc::new(ikigai_intray::space(f.state.path().join(TASKS))));
    let stolen = issue(
        &intray,
        Verb::Delete,
        "urn:space:tuples",
        &[],
        &Capability::root(),
    );
    assert!(stolen.is_ok(), "{stolen:?}");
    let refused = take(&f.kernel, &bobby_gonk(), &offer_iri("a"));
    assert!(
        matches!(refused, Err(Error::Conflict(ref m)) if m.contains("no receipt")),
        "{refused:?}"
    );
    assert_eq!(rd(&f.kernel, &[("state", "taken")]), "");
    let reused = out(&f.kernel, &plasma(), &offer("a"));
    assert!(matches!(reused, Err(Error::Conflict(_))), "{reused:?}");
    // And after a restart, from the feed's record of the name.
    let mut f = f;
    f.reopen();
    let reused = out(&f.kernel, &plasma(), &offer("a").replace("Do a.", "Do A."));
    assert!(matches!(reused, Err(Error::Conflict(_))), "{reused:?}");
}

#[test]
fn everything_survives_a_restart() {
    let mut f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    out(&f.kernel, &plasma(), &offer("b")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let before = (
        rd(&f.kernel, &[]),
        rd(&f.kernel, &[("state", "taken")]),
        feed(&f.kernel),
    );
    f.reopen();
    let after = (
        rd(&f.kernel, &[]),
        rd(&f.kernel, &[("state", "taken")]),
        feed(&f.kernel),
    );
    assert_eq!(before, after);
    let again = take(&f.kernel, &plasma(), &offer_iri("a"));
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    take(&f.kernel, &plasma(), &offer_iri("b")).unwrap();
    assert_eq!(kinds(&f.kernel), ["out", "out", "taken", "taken"]);
}

fn pending_cid(f: &Fixture) -> String {
    let inbox = f.state.path().join(TASKS).join("tuples").join("inbox");
    let mut names: Vec<String> = std::fs::read_dir(inbox)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
    names.pop().unwrap().trim_end_matches(".tuple").to_string()
}

fn drop_last_feed_line(f: &Fixture) {
    let path = f.state.path().join(TASKS).join("feed.log");
    let text = std::fs::read_to_string(&path).unwrap();
    let kept: Vec<&str> = text.lines().collect();
    let kept = kept[..kept.len() - 1].join("\n") + "\n";
    std::fs::write(&path, kept).unwrap();
}

/// A crash after the take but before its feed event: the receipt is committed, and the
/// feed logs it on the next open, in order.
#[test]
fn a_take_the_feed_missed_is_logged_on_open() {
    let mut f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let whole = feed(&f.kernel);
    drop_last_feed_line(&f);
    f.reopen();
    assert_eq!(feed(&f.kernel), whole);
}

/// A crash after intray's take but before the receipt was committed: the staged receipt is
/// committed on open (its tuple is gone, so the take happened), and the feed logs it.
#[test]
fn a_staged_receipt_whose_tuple_is_gone_is_committed_on_open() {
    let mut f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let cid = pending_cid(&f);
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let whole = feed(&f.kernel);
    let receipts = f.state.path().join(TASKS).join("receipts");
    let file = std::fs::read_dir(&receipts)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.is_file())
        .unwrap();
    let id = file.file_stem().unwrap().to_string_lossy().into_owned();
    std::fs::rename(
        &file,
        receipts.join(".taking").join(format!("{cid}.{id}.ttl")),
    )
    .unwrap();
    drop_last_feed_line(&f);
    f.reopen();
    assert_eq!(feed(&f.kernel), whole);
    let receipt = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    );
    assert!(receipt.contains(&bobby_gonk()), "{receipt}");
}

/// A crash after staging a receipt but before intray's take: the take never happened, so
/// the staged receipt is dropped and the tuple is still there to take.
#[test]
fn a_staged_receipt_whose_tuple_is_pending_is_dropped_on_open() {
    let mut f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let cid = pending_cid(&f);
    let staged = f
        .state
        .path()
        .join(TASKS)
        .join("receipts")
        .join(".taking")
        .join(format!("{cid}.0000000000000000000000002.ttl"));
    std::fs::write(&staged, sealed(f.config.path(), b"never read")).unwrap();
    f.reopen();
    assert!(!staged.exists());
    assert_eq!(rd(&f.kernel, &[]).lines().count(), 1);
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
}

/// The feed follows a task layer exactly as a channel, and one wait covers both.
#[test]
fn the_multiplexed_feed_covers_a_task_layer() {
    let f = fixture();
    say(
        &f.kernel,
        "talk-bobby",
        &participant("bobby"),
        &message("hello"),
    )
    .unwrap();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let both = cap(
        &participant("bobby"),
        &[
            "urn:cap:layer:feed:talk-bobby",
            "urn:cap:layer:feed:tasks-bobby",
        ],
    );
    let lines = source(
        &f.kernel,
        "urn:layers:feed",
        &[("after", "talk-bobby:0,tasks-bobby:0")],
        &both,
    );
    let kinds: Vec<(&str, &str)> = lines
        .lines()
        .map(|l| {
            let parts: Vec<&str> = l.split(' ').collect();
            (parts[0], parts[3])
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("talk-bobby", "say"),
            ("tasks-bobby", "out"),
            ("tasks-bobby", "taken")
        ]
    );
}

/// The definition says what the layer is, and what it is not: no `say` door on a tuplespace,
/// no `tuples` door on a channel.
#[test]
fn a_tuplespace_binds_its_own_doors_only() {
    let f = fixture();
    let def = source(&f.kernel, "urn:layer:tasks-bobby", &[], &Capability::root());
    assert!(def.contains("ik:layerModel \"tuplespace\""), "{def}");
    assert!(def.contains("urn:layer:tasks-bobby:tuples"), "{def}");
    assert!(!def.contains("ns#layer>") && !def.contains("ik:layer "));
    for absent in ["urn:layer:tasks-bobby:say", "urn:layer:talk:tuples"] {
        let refused = issue(&f.kernel, Verb::Source, absent, &[], &Capability::root());
        assert!(
            matches!(refused, Err(Error::Unresolved(_))),
            "{absent}: {refused:?}"
        );
    }
}

/// A cross join of `k` triple patterns whose FILTER reads every variable: about 7.5x the work
/// per pattern on a seven-triple tuple, ~300 bytes at k = 7 (ledger #1327). Nothing about its
/// size or nesting is unusual, so the store's limits pass it.
fn cross_join(k: usize) -> String {
    let patterns: String = (0..k).map(|i| format!("?s{i} ?p{i} ?o{i} . ")).collect();
    let all: Vec<String> = (0..k).map(|i| format!("STR(?o{i})")).collect();
    format!(
        "ASK {{ {patterns}FILTER(CONCAT({}) = \"never\") }}",
        all.join(", ")
    )
}

/// The tuples door's self-description, as JSON.
fn meta(f: &Fixture) -> String {
    text(
        &issue(
            &f.kernel,
            Verb::Meta,
            TUPLES,
            &[("as", "application/json")],
            &Capability::root(),
        )
        .expect("Meta"),
    )
}

/// The fixture with `tasks-bobby` opted in to templates (`match = true`).
fn matching_fixture() -> Fixture {
    let mut f = fixture();
    f.config.write(
        "layers/tasks-bobby.toml",
        "model = \"tuplespace\"\nlabel = \"Tasks for Bobby\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\naddressed = true\nmatch = true\n",
    );
    f.reopen();
    f
}

/// Ledger #1327: `rd match=` evaluated the caller's ASK with no time bound (until intray
/// 0.1.55), so a tuplespace layer refuses a template unless its definition says
/// `match = true`, and names the option. Before this, the same request was ACCEPTED
/// (asserted on an empty space, so the test stays fast: acceptance is decided before any
/// tuple is read).
#[test]
fn a_template_is_refused_unless_the_layer_allows_it() {
    let f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let template = cross_join(8);
    assert!(template.len() < 400, "{}", template.len());
    for args in [
        vec![("match", template.as_str())],
        vec![("match", "ASK { ?s ?p ?o }")],
        vec![("match", "ASK { ?s ?p ?o }"), ("state", "taken")],
        vec![("match", "ASK { ?s ?p ?o }"), ("tuple", &offer_iri("a"))],
    ] {
        let refused = issue(
            &f.kernel,
            Verb::Source,
            TUPLES,
            &args,
            &tasker(&participant("bobby")),
        );
        assert!(
            matches!(&refused, Err(Error::InvalidArgument { name, detail })
                if name == "match" && detail.contains("`match = true`")
                    && detail.contains("holds a core")),
            "{args:?}: {refused:?}"
        );
    }
    // Without a template, rd is unchanged: the list, one tuple, the receipts.
    assert_eq!(rd(&f.kernel, &[]).lines().count(), 1);
    assert!(rd(&f.kernel, &[("tuple", &offer_iri("a"))]).contains("Task a"));
    assert_eq!(rd(&f.kernel, &[("state", "taken")]), "");
    // And a layer that does not take a template does not offer one.
    let meta = meta(&f);
    assert!(!meta.contains("\"match\""), "{meta}");
    assert!(meta.contains("\"tuple\""), "{meta}");
}

/// Opted in, a template works, and the pathological one is still ACCEPTED: the option decides
/// WHO may send one; what one costs is bounded by the host's time budget
/// (`tests/match_budget.rs`, ledger #1362), and on an empty space nothing is evaluated.
#[test]
fn an_opted_in_layer_takes_a_template_and_offers_it() {
    let f = matching_fixture();
    let answered = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("match", &cross_join(8))],
        &tasker(&participant("bobby")),
    );
    assert!(answered.is_ok(), "{answered:?}");
    let meta = meta(&f);
    assert!(meta.contains("\"match\""), "{meta}");
}

/// Ledger #2017: a crash between intray's claim (the rename into `tuples/.taking/`) and its
/// removal leaves the claim behind after the layer has committed the take's receipt. Intray
/// puts a claim older than five minutes back in the inbox on its next `rd` or `take`, which
/// would make the tuple pending again beside its receipt (and the next open refuse it as
/// stored twice). The host owns the tree and records its takes itself, so it discards every
/// staged claim and drop at open, before serving: the take happened.
#[test]
fn a_claim_a_crashed_take_left_behind_is_discarded_at_open() {
    let mut f = fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let cid = pending_cid(&f);
    let space = f.state.path().join(TASKS).join("tuples");
    let stored = std::fs::read(space.join("inbox").join(format!("{cid}.tuple"))).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let before = (
        rd(&f.kernel, &[]),
        rd(&f.kernel, &[("state", "taken")]),
        feed(&f.kernel),
    );
    // Intray's claim name (`{id}.{millis}.{pid}.{n}.claim`), claimed at the epoch's first
    // millisecond: long past its five minutes.
    let claim = space.join(".taking").join(format!("{cid}.1.1.0.claim"));
    std::fs::write(&claim, &stored).unwrap();
    f.reopen();
    assert!(!claim.exists(), "the claim is discarded at open");
    let after = (
        rd(&f.kernel, &[]),
        rd(&f.kernel, &[("state", "taken")]),
        feed(&f.kernel),
    );
    assert_eq!(before, after);
    assert!(
        !space.join("inbox").join(format!("{cid}.tuple")).exists(),
        "never pending again"
    );
    let again = take(&f.kernel, &plasma(), &offer_iri("a"));
    assert!(matches!(again, Err(Error::Conflict(_))), "{again:?}");
    f.reopen();
    assert_eq!(rd(&f.kernel, &[]), before.0);
}

/// Ledger #2017: a pending tuple that does not open (sealed to a key this host does not hold,
/// planted past the host) refuses a `match=` with intray's typed `Conflict` naming it, where
/// the host's own copy of the match used to skip it as if it were absent.
#[test]
fn a_tuple_that_does_not_open_refuses_a_match_naming_it() {
    let f = matching_fixture();
    out(&f.kernel, &plasma(), &offer("a")).unwrap();
    let other = Scratch::new("other-key");
    key(other.path());
    let planted = sealed(other.path(), offer("x").as_bytes());
    let id = ikigai_intray::tuple_id(&planted);
    let inbox = f.state.path().join(TASKS).join("tuples").join("inbox");
    std::fs::write(inbox.join(format!("{id}.tuple")), planted).unwrap();
    let refused = issue(
        &f.kernel,
        Verb::Source,
        TUPLES,
        &[("match", "ASK { ?s ?p ?o }")],
        &tasker(&participant("bobby")),
    );
    assert!(
        matches!(refused, Err(Error::Conflict(ref m)) if m.contains(&id) && m.contains("does not decode")),
        "{refused:?}"
    );
}
