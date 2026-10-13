//! A write whose feed event fails after the write is stored (ledger #1218): the sequence
//! number it was stamped with is spent, so the next write must not reuse it, the feed must
//! log it before anything later, and the layer must reopen.
//!
//! The fault is injected from outside, with no hook in the library: a layer's feed file is
//! opened for every append, so swapping it for a directory makes exactly the next append
//! fail (EISDIR, which root gets too) while everything else the write does still succeeds.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::path::PathBuf;

use common::*;
use ikigai_core::{Capability, Error, Kernel};
use ikigai_layers::id;

fn feed_path(f: &Fixture, layer: &str) -> PathBuf {
    f.state.path().join(layer).join("feed.log")
}

/// Make the layer's next feed appends fail: the log is moved aside and a directory takes
/// its name.
fn break_feed(f: &Fixture, layer: &str) {
    let path = feed_path(f, layer);
    std::fs::rename(&path, path.with_extension("saved")).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("in-the-way"), "").unwrap();
}

/// Put the log back.
fn mend_feed(f: &Fixture, layer: &str) {
    let path = feed_path(f, layer);
    std::fs::remove_dir_all(&path).unwrap();
    std::fs::rename(path.with_extension("saved"), &path).unwrap();
}

/// The feed as `(seq, kind, iri)`.
fn events(kernel: &Kernel, layer: &str) -> Vec<(u64, String, String)> {
    source(
        kernel,
        &format!("urn:layer:{layer}:feed"),
        &[("after", "0")],
        &Capability::root(),
    )
    .lines()
    .map(|l| {
        let parts: Vec<&str> = l.split(' ').collect();
        (
            parts[0].parse().unwrap(),
            parts[2].to_string(),
            parts[3].to_string(),
        )
    })
    .collect()
}

fn seq_of_entry(iri: &str) -> u64 {
    id::seq_of(iri.rsplit(':').next().unwrap()).unwrap()
}

/// The entry IRI a failed `say` names in its error.
fn entry_in(error: &Error, layer: &str) -> String {
    let text = error.to_string();
    let prefix = format!("urn:layer:{layer}:entry:");
    let start = text.find(&prefix).unwrap_or_else(|| panic!("{text}"));
    text[start..start + prefix.len() + 23].to_string()
}

#[test]
fn a_say_whose_feed_event_failed_keeps_its_seq_and_is_logged_next() {
    let mut f = fixture();
    let bobby = participant("bobby");
    let one = say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    break_feed(&f, "talk");
    let failed = say(&f.kernel, "talk", &bobby, &message("two")).unwrap_err();
    assert!(matches!(failed, Error::Endpoint(_)), "{failed:?}");
    // The entry is stored, and the error says so: retrying would say it twice.
    let two = entry_in(&failed, "talk");
    assert_eq!(seq_of_entry(&two), 2);
    assert!(failed.to_string().contains("is stored"), "{failed}");
    mend_feed(&f, "talk");

    let three = say(&f.kernel, "talk", &bobby, &message("three")).unwrap();
    assert_eq!(seq_of_entry(&three), 3, "the next write must not reuse 2");
    let expected = vec![
        (1, "say".to_string(), one.clone()),
        (2, "say".to_string(), two.clone()),
        (3, "say".to_string(), three.clone()),
    ];
    assert_eq!(events(&f.kernel, "talk"), expected);

    f.reopen();
    assert_eq!(events(&f.kernel, "talk"), expected);
    let four = say(&f.kernel, "talk", &bobby, &message("four")).unwrap();
    assert_eq!(seq_of_entry(&four), 4);
}

#[test]
fn a_layer_whose_feed_stays_broken_takes_no_more_writes_and_still_reopens() {
    let mut f = fixture();
    let bobby = participant("bobby");
    say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    break_feed(&f, "talk");
    let failed = say(&f.kernel, "talk", &bobby, &message("two")).unwrap_err();
    let two = entry_in(&failed, "talk");
    // The owed event cannot be logged, so the next write is refused BEFORE it stores
    // anything: nothing else can claim seq 2, and nothing piles up unlogged.
    let refused = say(&f.kernel, "talk", &bobby, &message("three"));
    assert!(
        matches!(refused, Err(Error::Endpoint(ref m)) if m.contains("feed")),
        "{refused:?}"
    );
    let entries = std::fs::read_dir(f.state.path().join("talk/graphs"))
        .unwrap()
        .map(|d| std::fs::read_dir(d.unwrap().path()).unwrap().count())
        .sum::<usize>();
    assert_eq!(entries, 2, "the refused write stored nothing");
    mend_feed(&f, "talk");

    // A crash here: the reopen logs the stored entry from the record.
    f.reopen();
    let logged = events(&f.kernel, "talk");
    assert_eq!(logged.len(), 2);
    assert_eq!(logged[1], (2, "say".to_string(), two));
    let three = say(&f.kernel, "talk", &bobby, &message("three")).unwrap();
    assert_eq!(seq_of_entry(&three), 3);
}

#[test]
fn a_take_whose_feed_event_failed_is_a_take_and_its_seq_is_spent() {
    let mut f = fixture();
    let plasma = gonk();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    out(&f.kernel, &plasma, &offer("b")).unwrap();
    break_feed(&f, TASKS);
    let failed = take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap_err();
    assert!(matches!(failed, Error::Endpoint(_)), "{failed:?}");
    assert!(
        failed.to_string().contains(&bobby_gonk()),
        "the error names the take that happened: {failed}"
    );
    mend_feed(&f, TASKS);

    // The take happened: a retry by its taker finds its own receipt, and nobody else can
    // take the tuple, before and after a restart.
    let again = take(&f.kernel, &bobby_gonk(), &offer_iri("a"));
    assert!(
        matches!(again, Err(Error::Conflict(ref m)) if m.contains("already taken") && m.contains(&bobby_gonk())),
        "{again:?}"
    );
    let receipt = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    );
    assert!(receipt.contains(&bobby_gonk()), "{receipt}");

    out(&f.kernel, &plasma, &offer("c")).unwrap();
    let expected = vec![
        (1, "out".to_string(), offer_iri("a")),
        (2, "out".to_string(), offer_iri("b")),
        (3, "taken".to_string(), offer_iri("a")),
        (4, "out".to_string(), offer_iri("c")),
    ];
    assert_eq!(events(&f.kernel, TASKS), expected);
    f.reopen();
    assert_eq!(events(&f.kernel, TASKS), expected);
    let again = take(&f.kernel, &plasma, &offer_iri("a"));
    assert!(
        matches!(again, Err(Error::Conflict(ref m)) if m.contains("already taken")),
        "{again:?}"
    );
}

#[test]
fn two_takes_against_a_broken_feed_cannot_share_a_seq() {
    let mut f = fixture();
    let plasma = gonk();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    out(&f.kernel, &plasma, &offer("b")).unwrap();
    break_feed(&f, TASKS);
    assert!(take(&f.kernel, &bobby_gonk(), &offer_iri("a")).is_err());
    // The second take is refused before it takes anything: `b` stays pending.
    let refused = take(&f.kernel, &bobby_gonk(), &offer_iri("b"));
    assert!(
        matches!(refused, Err(Error::Endpoint(ref m)) if m.contains("feed")),
        "{refused:?}"
    );
    mend_feed(&f, TASKS);
    f.reopen();
    assert_eq!(
        events(&f.kernel, TASKS),
        vec![
            (1, "out".to_string(), offer_iri("a")),
            (2, "out".to_string(), offer_iri("b")),
            (3, "taken".to_string(), offer_iri("a")),
        ]
    );
    take(&f.kernel, &bobby_gonk(), &offer_iri("b")).unwrap();
    assert_eq!(events(&f.kernel, TASKS)[3].0, 4);
}

#[test]
fn an_out_whose_feed_event_failed_is_taken_back_and_its_seq_reused_safely() {
    let mut f = fixture();
    let plasma = gonk();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    break_feed(&f, TASKS);
    let failed = out(&f.kernel, &plasma, &offer("b"));
    assert!(matches!(failed, Err(Error::Endpoint(_))), "{failed:?}");
    mend_feed(&f, TASKS);
    // Taken back: nothing holds seq 2, so the next drop takes it, and `b` is pending
    // nowhere. Its subject was never logged, so it is free to drop again.
    out(&f.kernel, &plasma, &offer("b")).unwrap();
    let expected = vec![
        (1, "out".to_string(), offer_iri("a")),
        (2, "out".to_string(), offer_iri("b")),
    ];
    assert_eq!(events(&f.kernel, TASKS), expected);
    f.reopen();
    assert_eq!(events(&f.kernel, TASKS), expected);
}

/// A failed append can leave bytes behind (written, then the sync failed): the next append
/// starts from the last whole event, never after the stray bytes.
#[test]
fn a_stray_tail_from_a_failed_append_is_overwritten() {
    let mut f = fixture();
    let bobby = participant("bobby");
    say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    let path = feed_path(&f, "talk");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("2\t2026-10-10T00:00:09Z\tsay\turn:layer:talk:entry:x\tnobody\n3\t20");
    std::fs::write(&path, text).unwrap();
    let two = say(&f.kernel, "talk", &bobby, &message("two")).unwrap();
    assert_eq!(seq_of_entry(&two), 2);
    f.reopen();
    let logged = events(&f.kernel, "talk");
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(logged[1].2, two);
}

/// The crash after a failed append whose line DID reach the disk (written, then the sync
/// reported an error) and before the owed event was settled: the open finds the event logged
/// once, and the stored entry it names, and the sequence goes on from there.
#[test]
fn a_crash_after_a_failed_append_that_reached_the_disk_reopens() {
    let mut f = fixture();
    let bobby = participant("bobby");
    say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    break_feed(&f, "talk");
    let failed = say(&f.kernel, "talk", &bobby, &message("two")).unwrap_err();
    let two = entry_in(&failed, "talk");
    mend_feed(&f, "talk");
    let path = feed_path(&f, "talk");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str(&format!(
        "2\t2026-10-10T00:00:09.000Z\tsay\t{two}\t{bobby}\n"
    ));
    std::fs::write(&path, text).unwrap();
    f.reopen();
    let logged = events(&f.kernel, "talk");
    assert_eq!(logged.len(), 2, "{logged:?}");
    assert_eq!(logged[1].2, two);
    let three = say(&f.kernel, "talk", &bobby, &message("three")).unwrap();
    assert_eq!(seq_of_entry(&three), 3);
}

/// The receipt a take will commit, by id: the same operations on a twin fixture. The test
/// clock is deterministic, so the twin's take is stamped at the same instant with the same
/// sequence number, and mints the same id.
fn twin_receipt_id(steps: impl Fn(&Fixture)) -> String {
    let twin = fixture();
    steps(&twin);
    let receipts = twin.state.path().join(TASKS).join("receipts");
    let mut ids: Vec<String> = std::fs::read_dir(&receipts)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| p.file_stem().unwrap().to_str().unwrap().to_string())
        .collect();
    assert_eq!(ids.len(), 1, "{ids:?}");
    ids.pop().unwrap()
}

/// A take whose receipt cannot be committed (ledger #1221): intray's take has happened, so
/// the take stands. Its sequence number is spent, it is recorded, its receipt reads from
/// where it is staged, and the next open commits it.
///
/// The fault is injected from outside: the receipt's id is predicted on a twin fixture, and
/// a directory takes its name, so the commit (a rename onto it) fails with EISDIR.
#[test]
fn a_take_whose_receipt_could_not_be_committed_is_a_take_and_its_seq_is_spent() {
    let plasma = gonk();
    let id = twin_receipt_id(|f| {
        out(&f.kernel, &gonk(), &offer("a")).unwrap();
        out(&f.kernel, &gonk(), &offer("b")).unwrap();
        take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    });
    let mut f = fixture();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    out(&f.kernel, &plasma, &offer("b")).unwrap();
    let blocker = f
        .state
        .path()
        .join(TASKS)
        .join("receipts")
        .join(format!("{id}.ttl"));
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("in-the-way"), "").unwrap();

    let failed = take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap_err();
    assert!(matches!(failed, Error::Endpoint(_)), "{failed:?}");
    let message = failed.to_string();
    assert!(
        message.contains(&bobby_gonk()),
        "names the taker: {message}"
    );
    assert!(message.contains(&id), "names the receipt: {message}");

    // The take stands: a retry finds it, and its receipt reads (from the staged file).
    let again = take(&f.kernel, &bobby_gonk(), &offer_iri("a"));
    assert!(
        matches!(again, Err(Error::Conflict(ref m)) if m.contains("already taken")),
        "{again:?}"
    );
    let receipt_iri = format!("urn:layer:{TASKS}:receipt:{id}");
    let receipt = source(&f.kernel, &receipt_iri, &[], &tasker(&bobby_gonk()));
    assert!(receipt.contains(&bobby_gonk()), "{receipt}");
    let listed = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("a"))],
        &tasker(&bobby_gonk()),
    );
    assert_eq!(listed, receipt);

    // Its number is spent: the next write carries the one after it.
    out(&f.kernel, &plasma, &offer("c")).unwrap();
    let expected = vec![
        (1, "out".to_string(), offer_iri("a")),
        (2, "out".to_string(), offer_iri("b")),
        (3, "taken".to_string(), offer_iri("a")),
        (4, "out".to_string(), offer_iri("c")),
    ];
    assert_eq!(events(&f.kernel, TASKS), expected);

    // The obstacle is cleared and the host restarts: the open commits the staged receipt.
    std::fs::remove_dir_all(&blocker).unwrap();
    f.reopen();
    assert!(blocker.is_file(), "the staged receipt is committed on open");
    assert_eq!(events(&f.kernel, TASKS), expected);
    assert_eq!(
        source(&f.kernel, &receipt_iri, &[], &tasker(&bobby_gonk())),
        receipt
    );
    let again = take(&f.kernel, &plasma, &offer_iri("a"));
    assert!(
        matches!(again, Err(Error::Conflict(ref m)) if m.contains("already taken")),
        "{again:?}"
    );
}

/// A write refused because the feed still cannot log an owed event names that event, so the
/// operator knows which stored write is waiting (ledger #1221).
#[test]
fn a_write_refused_while_the_feed_is_broken_names_the_owed_event() {
    let mut f = fixture();
    let bobby = participant("bobby");
    say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    break_feed(&f, "talk");
    let failed = say(&f.kernel, "talk", &bobby, &message("two")).unwrap_err();
    let two = entry_in(&failed, "talk");
    let refused = say(&f.kernel, "talk", &bobby, &message("three")).unwrap_err();
    let message = refused.to_string();
    assert!(message.contains(&two), "names the owed entry: {message}");
    assert!(message.contains("ik:seq 2"), "and its number: {message}");
    mend_feed(&f, "talk");

    let plasma = gonk();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    break_feed(&f, TASKS);
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap_err();
    let refused = out(&f.kernel, &plasma, &offer("b")).unwrap_err();
    let message = refused.to_string();
    assert!(message.contains("taken"), "names the owed take: {message}");
    assert!(message.contains(&offer_iri("a")), "{message}");
    assert!(message.contains("ik:seq 2"), "{message}");
    mend_feed(&f, TASKS);
    f.reopen();
}
