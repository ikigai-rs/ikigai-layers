//! A feed's epoch: the door's name for its feed's incarnation (ledger #4114, the restored-door
//! case LF-5 of ledger #3559). A cursor from another epoch is refused as Conflict, the way a
//! cursor past the head is, so a follower of a restored or replaced door never silently skips
//! the events the restored door numbered again.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::path::Path;

use common::*;
use ikigai_core::{Error, Verb};
use ikigai_layers::Host;

fn feed_json(f: &Fixture, args: &[(&str, &str)]) -> Result<serde_json::Value, Error> {
    let mut args = args.to_vec();
    args.push(("as", "application/json"));
    issue(
        &f.kernel,
        Verb::Source,
        "urn:layer:talk:feed",
        &args,
        &member(&participant("bobby")),
    )
    .map(|r| serde_json::from_str(&text(&r)).unwrap())
}

fn multi_json(f: &Fixture, args: &[(&str, &str)]) -> Result<serde_json::Value, Error> {
    let mut args = args.to_vec();
    args.push(("as", "application/json"));
    issue(
        &f.kernel,
        Verb::Source,
        "urn:layers:feed",
        &args,
        &member(&participant("bobby")),
    )
    .map(|r| serde_json::from_str(&text(&r)).unwrap())
}

fn says(f: &Fixture, n: usize, what: &str) {
    for i in 0..n {
        say(
            &f.kernel,
            "talk",
            &participant("brian"),
            &message(&format!("{what}{i}")),
        )
        .unwrap();
    }
}

/// Copy a state directory as a backup does: every file, the epoch file included.
fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else if entry.file_name() != "host.lock" {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// A host over `state`, with `f`'s configuration.
fn door_over(f: &Fixture, state: &Path) -> Fixture {
    let config = Scratch::new("config");
    write_config(&config);
    std::fs::copy(
        f.config.path().join("state.key"),
        config.path().join("state.key"),
    )
    .unwrap();
    let host = Host::open(config.path(), state, &key(config.path())).expect("the second door");
    let kernel = kernel_over(&host);
    Fixture {
        config,
        state: Scratch::new("unused"),
        host,
        kernel,
    }
}

#[test]
fn the_json_faces_carry_the_epoch_and_it_survives_a_restart() {
    let mut f = fixture();
    says(&f, 1, "x");
    let epoch = f.host.epoch().to_string();
    assert_eq!(epoch.len(), 32, "{epoch}");
    assert!(epoch.bytes().all(|b| b.is_ascii_hexdigit()), "{epoch}");
    let one = feed_json(&f, &[("after", "0")]).unwrap();
    assert_eq!(one["epoch"], epoch.as_str());
    let all = multi_json(&f, &[("after", "talk:0")]).unwrap();
    assert_eq!(all["epoch"], epoch.as_str());
    f.reopen();
    assert_eq!(f.host.epoch(), epoch, "a restart is the same incarnation");
    let again = feed_json(&f, &[("after", "1"), ("epoch", &epoch)]).unwrap();
    assert_eq!(again["epoch"], epoch.as_str());
    assert_eq!(again["last"], 1);
}

#[test]
fn a_cursor_from_another_hosts_feed_is_refused_even_below_the_head() {
    let a = fixture();
    says(&a, 2, "a");
    let seen = feed_json(&a, &[("after", "0")]).unwrap();
    let epoch = seen["epoch"].as_str().unwrap().to_string();
    // Another door, over a state of its own, numbers its own events from 1.
    let b = fixture();
    says(&b, 5, "b");
    let refused = feed_json(&b, &[("after", "2"), ("epoch", &epoch)]).unwrap_err();
    let Error::Conflict(why) = refused else {
        panic!("{refused:?}")
    };
    assert!(why.contains(&epoch), "{why}");
    assert!(why.contains(b.host.epoch()), "{why}");
    assert!(why.contains("layer `talk`"), "{why}");
    assert!(why.contains("resynchronize from 0"), "{why}");
    let refused = multi_json(&b, &[("after", "talk:2"), ("epoch", &epoch)]).unwrap_err();
    assert!(matches!(refused, Error::Conflict(_)), "{refused:?}");
    // From 0 with the new epoch is the resynchronization.
    let fresh = feed_json(&b, &[("after", "0"), ("epoch", b.host.epoch())]).unwrap();
    assert_eq!(fresh["last"], 5);
}

/// LF-5: a door restored from a backup numbers again the events written after the backup. A
/// follower that read past the backup's head, once the restored door has written as many
/// again, cannot tell by the sequence alone: before the epoch it was answered only what came
/// after its cursor and silently missed the restored door's own events at the numbers it had
/// seen. The restore names a new epoch ([`Host::renew_epoch`]), and the cursor is refused.
#[test]
fn a_cursor_from_before_a_restore_is_refused_once_the_restore_names_a_new_epoch() {
    let a = fixture();
    says(&a, 2, "before-backup-");
    let backup = Scratch::new("backup");
    copy_dir(a.state.path(), backup.path());
    says(&a, 2, "lost-");
    let seen = feed_json(&a, &[("after", "0")]).unwrap();
    assert_eq!(seen["last"], 4);
    let epoch = seen["epoch"].as_str().unwrap().to_string();

    let renewed = Host::renew_epoch(backup.path()).unwrap();
    assert_ne!(renewed, epoch);
    let restored = door_over(&a, backup.path());
    assert_eq!(restored.host.epoch(), renewed);
    says(&restored, 3, "after-restore-");
    let refused = feed_json(&restored, &[("after", "4"), ("epoch", &epoch)]).unwrap_err();
    assert!(matches!(refused, Error::Conflict(_)), "{refused:?}");
    let replay = feed_json(&restored, &[("after", "0"), ("epoch", &renewed)]).unwrap();
    assert_eq!(replay["last"], 5);
    assert_eq!(replay["events"].as_array().unwrap().len(), 5);
}

#[test]
fn a_restore_that_names_no_new_epoch_keeps_the_old_one() {
    // What the restore step is for: a backup carries the epoch with every other byte, so a
    // restored state is the same incarnation until the operator names a new one.
    let a = fixture();
    says(&a, 1, "x");
    let backup = Scratch::new("backup");
    copy_dir(a.state.path(), backup.path());
    let epoch = a.host.epoch().to_string();
    let restored = door_over(&a, backup.path());
    assert_eq!(restored.host.epoch(), epoch);
}

#[test]
fn an_epoch_is_renewed_only_while_no_host_holds_the_state() {
    let f = fixture();
    let refused = Host::renew_epoch(f.state.path()).unwrap_err();
    assert!(refused.to_string().contains("pid"), "{refused}");
}

#[test]
fn a_malformed_epoch_is_refused_as_an_argument() {
    let f = fixture();
    let refused = feed_json(&f, &[("after", "0"), ("epoch", "")]).unwrap_err();
    assert!(
        matches!(&refused, Error::InvalidArgument { name, .. } if name == "epoch"),
        "{refused:?}"
    );
}

#[test]
fn a_read_without_an_epoch_is_answered_as_before() {
    let f = fixture();
    says(&f, 1, "x");
    let one = feed_json(&f, &[("after", "0")]).unwrap();
    assert_eq!(one["last"], 1);
}

#[test]
fn the_binary_prints_and_renews_the_epoch() {
    use std::process::Command;
    let bin = env!("CARGO_BIN_EXE_ikigai-layers");
    let a = fixture();
    let backup = Scratch::new("backup");
    copy_dir(a.state.path(), backup.path());
    let shown = Command::new(bin)
        .args(["epoch", backup.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(shown.status.success(), "{shown:?}");
    assert_eq!(
        String::from_utf8(shown.stdout).unwrap(),
        format!("epoch {}\n", a.host.epoch())
    );
    let renewed = Command::new(bin)
        .args(["epoch", backup.path().to_str().unwrap(), "--renew"])
        .output()
        .unwrap();
    assert!(renewed.status.success(), "{renewed:?}");
    let renewed = String::from_utf8(renewed.stdout).unwrap();
    assert!(renewed.starts_with("epoch "), "{renewed}");
    assert!(!renewed.contains(a.host.epoch()), "{renewed}");
    // The running host's own directory is refused, naming the holder.
    let held = Command::new(bin)
        .args(["epoch", a.state.path().to_str().unwrap(), "--renew"])
        .output()
        .unwrap();
    assert_eq!(held.status.code(), Some(1), "{held:?}");
    assert!(String::from_utf8(held.stderr).unwrap().contains("pid"));
    // A directory no host opened is refused, and nothing is created in it.
    let none = Scratch::new("none");
    let refused = Command::new(bin)
        .args(["epoch", none.path().to_str().unwrap(), "--renew"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(!none.path().join("feed.epoch").exists());
}
