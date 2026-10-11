//! Feeds: gapless, cursor-resumable, and a long poll that wakes on the next event.

mod common;

use std::time::{Duration, Instant};

use common::*;
use ikigai_core::{Error, Verb};

fn feed(f: &Fixture, layer: &str, args: &[(&str, &str)]) -> Result<String, Error> {
    issue(
        &f.kernel,
        Verb::Source,
        &format!("urn:layer:{layer}:feed"),
        args,
        &member(&participant("bobby")),
    )
    .map(|r| text(&r))
}

#[test]
fn the_sequence_is_gapless_and_resumable() {
    let f = fixture();
    let bobby = participant("bobby");
    let mut iris = Vec::new();
    for n in 0..5 {
        iris.push(say(&f.kernel, "talk", &bobby, &message(&format!("m{n}"))).unwrap());
    }
    let all = feed(&f, "talk", &[("after", "0")]).unwrap();
    let seqs: Vec<u64> = all
        .lines()
        .map(|l| l.split(' ').next().unwrap().parse().unwrap())
        .collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    let rest = feed(&f, "talk", &[("after", "3")]).unwrap();
    assert_eq!(rest.lines().count(), 2);
    assert!(rest.contains(&iris[3]) && rest.contains(&iris[4]));
    // Each layer has its own sequence.
    say(&f.kernel, "notes", &bobby, &message("n")).unwrap();
    assert!(feed(&f, "notes", &[("after", "0")])
        .unwrap()
        .starts_with("1 "));
}

#[test]
fn after_is_required_and_a_cursor_from_the_future_is_refused() {
    let f = fixture();
    assert!(matches!(feed(&f, "talk", &[]), Err(Error::MissingArgument(ref a)) if a == "after"));
    assert!(matches!(
        feed(&f, "talk", &[("after", "9")]),
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        feed(&f, "talk", &[("after", "0"), ("wait", "61")]),
        Err(Error::InvalidArgument { ref name, .. }) if name == "wait"
    ));
}

#[test]
fn a_wait_returns_on_the_next_event() {
    let f = fixture();
    let started = Instant::now();
    let got = std::thread::scope(|s| {
        let waiter = s.spawn(|| feed(&f, "talk", &[("after", "0"), ("wait", "20")]));
        std::thread::sleep(Duration::from_millis(300));
        say(
            &f.kernel,
            "talk",
            &participant("brian"),
            &message("wake up"),
        )
        .unwrap();
        waiter.join().unwrap().unwrap()
    });
    assert!(got.starts_with("1 "), "{got}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "woke on the event, not the timer: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_wait_with_nothing_to_say_ends_at_its_bound() {
    let f = fixture();
    let started = Instant::now();
    let got = feed(&f, "talk", &[("after", "0"), ("wait", "1")]).unwrap();
    assert_eq!(got, "");
    assert!(started.elapsed() >= Duration::from_millis(900));
    // An event in ANOTHER layer is not this layer's event: the wait goes on to its bound.
    let started = Instant::now();
    let got = std::thread::scope(|s| {
        let waiter = s.spawn(|| feed(&f, "talk", &[("after", "0"), ("wait", "1")]));
        std::thread::sleep(Duration::from_millis(200));
        say(
            &f.kernel,
            "notes",
            &participant("brian"),
            &message("elsewhere"),
        )
        .unwrap();
        waiter.join().unwrap().unwrap()
    });
    assert_eq!(got, "");
    assert!(started.elapsed() >= Duration::from_millis(900));
}

#[test]
fn the_json_face_carries_the_last_sequence() {
    let f = fixture();
    say(&f.kernel, "talk", &participant("bobby"), &message("x")).unwrap();
    let json: serde_json::Value = serde_json::from_str(
        &feed(&f, "talk", &[("after", "0"), ("as", "application/json")]).unwrap(),
    )
    .unwrap();
    assert_eq!(json["last"], 1);
    assert_eq!(json["events"][0]["kind"], "say");
    assert_eq!(json["events"][0]["by"], participant("bobby"));
}

fn multi(f: &Fixture, args: &[(&str, &str)], scopes: &[&str]) -> Result<String, Error> {
    issue(
        &f.kernel,
        Verb::Source,
        "urn:layers:feed",
        args,
        &cap(&participant("bobby"), scopes),
    )
    .map(|r| text(&r))
}

#[test]
fn one_wait_over_several_layers() {
    let f = fixture();
    let brian = participant("brian");
    say(&f.kernel, "talk", &brian, &message("t1")).unwrap();
    say(&f.kernel, "talk-bobby", &brian, &message("b1")).unwrap();
    say(&f.kernel, "talk", &brian, &message("t2")).unwrap();
    let both = ["urn:cap:layer:feed:talk", "urn:cap:layer:feed:talk-bobby"];
    let got = multi(&f, &[("after", "talk:1,talk-bobby:0")], &both).unwrap();
    let lines: Vec<&str> = got.lines().collect();
    assert_eq!(lines.len(), 2, "{got}");
    assert!(lines.iter().any(|l| l.starts_with("talk 2 ")));
    assert!(lines.iter().any(|l| l.starts_with("talk-bobby 1 ")));
    let json: serde_json::Value = serde_json::from_str(
        &multi(
            &f,
            &[("after", "talk:0,talk-bobby:0"), ("as", "application/json")],
            &both,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(json["cursor"], "talk:2,talk-bobby:1");

    // A wait over both wakes on either.
    let got = std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            multi(
                &f,
                &[("after", "talk:2,talk-bobby:1"), ("wait", "20")],
                &both,
            )
        });
        std::thread::sleep(Duration::from_millis(300));
        say(&f.kernel, "talk-bobby", &brian, &message("b2")).unwrap();
        waiter.join().unwrap().unwrap()
    });
    assert!(got.starts_with("talk-bobby 2 "), "{got}");
}

#[test]
fn every_layer_in_the_cursor_needs_its_own_exact_grant() {
    let f = fixture();
    let refused = multi(
        &f,
        &[("after", "talk:0,talk-bobby:0")],
        &["urn:cap:layer:feed:talk"],
    );
    assert!(
        matches!(refused, Err(Error::Denied(ref m)) if m.contains("talk-bobby")),
        "{refused:?}"
    );
    // An unknown layer is refused exactly like an unheld one: the answer names no layer
    // the caller could not already name.
    let unknown = multi(
        &f,
        &[("after", "talk:0,secret:0")],
        &["urn:cap:layer:feed:talk"],
    );
    assert!(
        matches!(unknown, Err(Error::Denied(ref m)) if m.contains("no feed grant on layer `secret`"))
    );
    // And nothing at all of the family: the kernel's floor refuses before the endpoint.
    let none = multi(&f, &[("after", "talk:0")], &["urn:cap:layer:read:talk"]);
    assert!(matches!(none, Err(Error::Denied(_))), "{none:?}");
}
