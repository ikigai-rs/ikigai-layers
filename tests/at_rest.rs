//! Layer state sealed at rest (ledger #2011): every state file is age ciphertext sealed to the
//! host's key; a plaintext directory is refused until one open migrates it; a wrong key opens
//! nothing; files are `0600` and directories `0700`.

// Every test here drives a layer host (ledger #1403: a participant build has none).
#![cfg(feature = "host")]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use ikigai_core::Capability;
use ikigai_layers::at_rest::{KEY_CHECK, MAGIC};
use ikigai_layers::{Host, Migration, StateKey};

/// What a sealed feed line begins with: the base64 of the age header's first line.
const LINE_MAGIC: &str = "YWdlLWVuY3J5cHRpb24ub3JnL3Yx";

/// A message body and an offer title that must never appear on disk in the clear.
const BODY: &str = "canary-body-2011";
const TITLE: &str = "Task canary";

fn offer_named(name: &str) -> String {
    offer(name).replace(&format!("Task {name}"), TITLE)
}

/// Every regular file under `dir`, recursively.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                out.push(entry.path());
            }
        }
    }
    out.sort();
    out
}

/// The host's own lock file holds its pid, not layer state.
fn is_lock(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == "host.lock")
}

/// No state file holds plaintext: each non-empty one is age (a feed, line by line), none
/// parses as Turtle, and none contains a word the layers were sent.
fn assert_sealed(state: &Path) {
    let mut checked = 0;
    for file in files(state) {
        if is_lock(&file) {
            continue;
        }
        let bytes = std::fs::read(&file).unwrap();
        if bytes.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for word in [BODY, TITLE, "ikigai-rs/ikigai-layers", "prov#"] {
            assert!(!text.contains(word), "{} holds `{word}`", file.display());
        }
        if file.file_name().is_some_and(|n| n == "feed.log") {
            for line in text.lines() {
                assert!(line.starts_with(LINE_MAGIC), "{}: {line}", file.display());
                assert!(!line.contains('\t'), "{}: {line}", file.display());
            }
        } else {
            assert!(bytes.starts_with(MAGIC), "{} is not age", file.display());
        }
        assert!(
            ikigai_layers::rdf::parse_turtle(&text).is_err(),
            "{} parses as Turtle",
            file.display()
        );
        checked += 1;
    }
    assert!(checked > 0, "nothing was checked");
}

/// Swap the fixture's host for an empty one over a scratch directory, so the state directory
/// is free (its lock goes with the host), and answer the scratch, which must outlive it.
fn park(f: &mut Fixture) -> Scratch {
    let between = Scratch::new("parked");
    let empty = Host::open(
        between.path(),
        between.path().join("state"),
        &key(between.path()),
    )
    .expect("empty");
    f.kernel = kernel_over(&empty);
    f.host = empty;
    between
}

fn open_with(f: &Fixture, key: &StateKey, migration: Migration) -> Result<Arc<Host>, String> {
    Host::open_with(f.config.path(), f.state.path(), key, migration).map_err(|e| e.0)
}

/// The refusal of an open that must fail.
fn refusal(opened: Result<Arc<Host>, String>) -> String {
    match opened {
        Ok(_) => panic!("the open succeeded"),
        Err(e) => e,
    }
}

fn install(f: &mut Fixture, host: Arc<Host>) {
    f.kernel = kernel_over(&host);
    f.host = host;
}

/// The identity the fixture's key file holds.
fn identity(config: &Path) -> age::x25519::Identity {
    let text = std::fs::read_to_string(config.join("state.key")).unwrap();
    text.lines()
        .find(|l| l.starts_with("AGE-SECRET-KEY-"))
        .unwrap()
        .parse()
        .unwrap()
}

/// Turn a sealed state directory into what a host before 0.3.0 left: every file opened in
/// place, each feed line opened, each pending tuple renamed to the id of its plaintext, and no
/// key check.
fn unseal_all(state: &Path, identity: &age::x25519::Identity) {
    use base64::Engine;
    std::fs::remove_file(state.join(KEY_CHECK)).unwrap();
    for file in files(state) {
        let bytes = std::fs::read(&file).unwrap();
        if is_lock(&file) || bytes.is_empty() {
            continue;
        }
        if file.file_name().is_some_and(|n| n == "feed.log") {
            let text: String = String::from_utf8(bytes)
                .unwrap()
                .lines()
                .map(|line| {
                    let sealed = base64::engine::general_purpose::STANDARD
                        .decode(line)
                        .unwrap();
                    let opened = age::decrypt(identity, &sealed).unwrap();
                    String::from_utf8(opened).unwrap() + "\n"
                })
                .collect();
            std::fs::write(&file, text).unwrap();
            continue;
        }
        let opened = age::decrypt(identity, &bytes).unwrap();
        if file.extension().is_some_and(|x| x == "tuple") {
            std::fs::remove_file(&file).unwrap();
            let id = ikigai_intray::tuple_id(&opened);
            std::fs::write(file.with_file_name(format!("{id}.tuple")), opened).unwrap();
        } else {
            std::fs::write(&file, opened).unwrap();
        }
    }
}

fn feed_of(f: &Fixture, layer: &str) -> String {
    source(
        &f.kernel,
        &format!("urn:layer:{layer}:feed"),
        &[("after", "0")],
        &Capability::root(),
    )
}

/// Everything a reader sees of the fixture's layers, to compare before and after.
fn view(f: &Fixture) -> Vec<String> {
    let root = Capability::root();
    vec![
        feed_of(f, "talk"),
        feed_of(f, TASKS),
        source(&f.kernel, "urn:layer:talk:entries", &[], &root),
        source(&f.kernel, TUPLES, &[("as", "text/turtle")], &root),
        source(
            &f.kernel,
            TUPLES,
            &[("state", "taken"), ("as", "text/turtle")],
            &root,
        ),
    ]
}

/// Some of everything: two entries, two offers, one of them taken.
fn activity(f: &Fixture) -> String {
    say(&f.kernel, "talk", &participant("bobby"), &message(BODY)).unwrap();
    say(&f.kernel, "talk", &participant("brian"), &message("second")).unwrap();
    out(&f.kernel, &gonk(), &offer_named("a")).unwrap();
    out(&f.kernel, &gonk(), &offer_named("b")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap()
}

/// A fixture whose task layer evaluates `match=` templates.
fn matching_fixture() -> Fixture {
    let mut f = fixture();
    f.config.write(
        "layers/tasks-bobby.toml",
        "model = \"tuplespace\"\nlabel = \"Tasks for Bobby\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\naddressed = true\nmatch = true\n",
    );
    f.reopen();
    f
}

#[test]
fn a_channel_entry_is_sealed_on_disk_and_reads_back() {
    let f = fixture();
    let entry = say(&f.kernel, "talk", &participant("bobby"), &message(BODY)).unwrap();
    let id = entry.rsplit(':').next().unwrap();
    let file = f
        .state
        .path()
        .join("talk/graphs/bobby")
        .join(format!("{id}.ttl"));
    let bytes = std::fs::read(&file).unwrap();
    assert!(bytes.starts_with(MAGIC));
    assert!(!String::from_utf8_lossy(&bytes).contains(BODY));
    let read = source(
        &f.kernel,
        "urn:layer:talk:entries",
        &[],
        &member(&participant("bobby")),
    );
    assert!(read.contains(BODY), "{read}");
    assert_sealed(f.state.path());
}

#[test]
fn a_tuple_and_its_receipt_are_sealed_on_disk_and_read_back() {
    let f = matching_fixture();
    out(&f.kernel, &gonk(), &offer_named("a")).unwrap();
    out(&f.kernel, &gonk(), &offer("b")).unwrap();
    let inbox = f.state.path().join(TASKS).join("tuples/inbox");
    assert_eq!(files(&inbox).len(), 2);
    assert_sealed(f.state.path());
    let reader = tasker(&participant("bobby"));
    let one = source(&f.kernel, TUPLES, &[("tuple", &offer_iri("a"))], &reader);
    assert!(one.contains(TITLE), "{one}");
    // A template is matched against the opened tuples.
    let ask = format!("ASK {{ ?o <http://purl.org/dc/terms/title> \"{TITLE}\" }}");
    let hits = source(&f.kernel, TUPLES, &[("match", &ask)], &reader);
    assert_eq!(hits.lines().count(), 1, "{hits}");
    assert!(hits.contains(&offer_iri("a")), "{hits}");
    let taken = take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    assert!(taken.contains(TITLE), "{taken}");
    let receipt = source(
        &f.kernel,
        TUPLES,
        &[("state", "taken"), ("tuple", &offer_iri("a"))],
        &reader,
    );
    assert!(
        receipt.contains(TITLE) && receipt.contains(&bobby_gonk()),
        "{receipt}"
    );
    assert_sealed(f.state.path());
}

#[test]
fn a_restart_keeps_the_feed_and_the_sequence() {
    let mut f = fixture();
    activity(&f);
    let before = view(&f);
    assert_eq!(feed_of(&f, "talk").lines().count(), 2);
    assert_eq!(feed_of(&f, TASKS).lines().count(), 3);
    f.reopen();
    assert_eq!(view(&f), before);
    let third = say(&f.kernel, "talk", &participant("bobby"), &message("third")).unwrap();
    assert_eq!(
        ikigai_layers::id::seq_of(third.rsplit(':').next().unwrap()),
        Some(3)
    );
    assert_sealed(f.state.path());
}

#[test]
fn a_plaintext_directory_is_refused_then_migrated_in_place() {
    let mut f = matching_fixture();
    activity(&f);
    let before = view(&f);
    let parked = park(&mut f);
    unseal_all(f.state.path(), &identity(f.config.path()));
    // What a host before this one left: plaintext Turtle and feed lines.
    let entries: Vec<PathBuf> = files(&f.state.path().join("talk/graphs"));
    let plain = std::fs::read_to_string(&entries[0]).unwrap();
    assert!(ikigai_layers::rdf::parse_turtle(&plain).is_ok(), "{plain}");
    let state = f.state.path().to_path_buf();
    assert!(
        std::panic::catch_unwind(|| assert_sealed(&state)).is_err(),
        "the scan finds plaintext when there is some"
    );
    let key = key(f.config.path());

    let refused = refusal(open_with(&f, &key, Migration::None));
    assert!(
        refused.contains("plaintext") && refused.contains("Migration::Reseal"),
        "{refused}"
    );
    let host = open_with(&f, &key, Migration::Reseal).expect("the migration");
    install(&mut f, host);
    drop(parked);
    assert_sealed(f.state.path());
    assert!(f.state.path().join(KEY_CHECK).exists());
    assert_eq!(view(&f), before);
    // And it works as before: a template, a take, a new entry, and a plain reopen.
    let ask = format!("ASK {{ ?o <http://purl.org/dc/terms/title> \"{TITLE}\" }}");
    let hits = source(
        &f.kernel,
        TUPLES,
        &[("match", &ask)],
        &tasker(&participant("bobby")),
    );
    assert!(hits.contains(&offer_iri("b")), "{hits}");
    take(&f.kernel, &bobby_gonk(), &offer_iri("b")).unwrap();
    say(&f.kernel, "talk", &participant("bobby"), &message("after")).unwrap();
    f.reopen();
    assert_eq!(feed_of(&f, TASKS).lines().count(), 4);
    assert_eq!(feed_of(&f, "talk").lines().count(), 3);
    assert_sealed(f.state.path());
}

#[test]
fn an_interrupted_migration_is_resumed() {
    use base64::Engine;
    let mut f = fixture();
    activity(&f);
    let before = view(&f);
    let parked = park(&mut f);
    let me = identity(f.config.path());
    unseal_all(f.state.path(), &me);
    // A run that stopped part-way: one entry already sealed, the pending tuple stored again
    // sealed but its plaintext copy not yet taken, the feed's first line sealed and the rest
    // not, and a replacement left half-written.
    let state = f.state.path();
    let entry = files(&state.join("talk/graphs"))[0].clone();
    let plain = std::fs::read(&entry).unwrap();
    std::fs::write(&entry, sealed(f.config.path(), &plain)).unwrap();
    let inbox = state.join(TASKS).join("tuples/inbox");
    let tuple = std::fs::read(&files(&inbox)[0]).unwrap();
    let copy = sealed(f.config.path(), &tuple);
    let id = ikigai_intray::tuple_id(&copy);
    std::fs::write(inbox.join(format!("{id}.tuple")), copy).unwrap();
    let feed = state.join("talk/feed.log");
    let text = std::fs::read_to_string(&feed).unwrap();
    let mut lines = text.lines();
    let first = sealed_line(f.config.path(), lines.next().unwrap());
    let rest: String = lines.map(|l| format!("{l}\n")).collect();
    std::fs::write(&feed, format!("{first}\n{rest}")).unwrap();
    let half = sealed(f.config.path(), b"a replacement that never finished");
    std::fs::write(
        state.join(TASKS).join(".feed.log.partial"),
        &half[..half.len() / 2],
    )
    .unwrap();
    assert_eq!(
        files(&inbox).len(),
        2,
        "the tuple twice, plaintext and sealed"
    );
    let key = key(f.config.path());

    assert!(
        open_with(&f, &key, Migration::None).is_err(),
        "plaintext remains"
    );
    let host = open_with(&f, &key, Migration::Reseal).expect("the resumed migration");
    install(&mut f, host);
    drop(parked);
    assert_eq!(view(&f), before);
    assert_eq!(files(&inbox).len(), 1, "the tuple once, not twice");
    // Every feed line now opens with the current key alone.
    let reopened = std::fs::read_to_string(&feed).unwrap();
    for line in reopened.lines() {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(line)
            .unwrap();
        age::decrypt(&me, &bytes).unwrap();
    }
    assert_sealed(f.state.path());
    // A migration run again changes nothing.
    let parked = park(&mut f);
    let host = open_with(&f, &key, Migration::Reseal).expect("an idempotent migration");
    install(&mut f, host);
    drop(parked);
    assert_eq!(view(&f), before);
}

#[test]
fn a_wrong_key_is_refused_at_open() {
    let mut f = fixture();
    let parked = park(&mut f);
    // An EMPTY layer is refused too: the key check, not a record, is what says.
    let other = Scratch::new("other-key");
    let wrong = key(other.path());
    let refused = refusal(open_with(&f, &wrong, Migration::None));
    assert!(
        refused.contains("does not open this state directory") && refused.contains("state.key"),
        "{refused}"
    );
    assert!(open_with(&f, &wrong, Migration::Reseal).is_err());
    let host = open_with(&f, &key(f.config.path()), Migration::None).expect("the right key");
    install(&mut f, host);
    drop(parked);

    // With records and no key check, the first record refuses the wrong key.
    activity(&f);
    let parked = park(&mut f);
    std::fs::remove_file(f.state.path().join(KEY_CHECK)).unwrap();
    let refused = refusal(open_with(&f, &wrong, Migration::None));
    assert!(refused.contains("sealed to a key"), "{refused}");
    // And it wrote no key check of its own.
    assert!(!f.state.path().join(KEY_CHECK).exists());
    let host = open_with(&f, &key(f.config.path()), Migration::None).expect("the right key");
    install(&mut f, host);
    drop(parked);
    assert!(f.state.path().join(KEY_CHECK).exists());
}

#[test]
fn a_missing_or_malformed_identity_is_refused_naming_it() {
    let dir = Scratch::new("bad-keys");
    let missing = StateKey::load(dir.path().join("absent.key")).unwrap_err().0;
    assert!(
        missing.contains("absent.key") && missing.contains("does not exist"),
        "{missing}"
    );
    dir.write("empty.key", "# nothing here\n\n");
    let empty = StateKey::load(dir.path().join("empty.key")).unwrap_err().0;
    assert!(empty.contains("holds no age identity"), "{empty}");
    dir.write(
        "bad.key",
        "# a comment\nAGE-SECRET-KEY-1NOTAREALKEYATALLBUTSECRETLOOKING\n",
    );
    let bad = StateKey::load(dir.path().join("bad.key")).unwrap_err().0;
    assert!(bad.contains("line 2"), "{bad}");
    assert!(
        !bad.contains("NOTAREALKEY"),
        "the line is never echoed: {bad}"
    );
}

#[test]
fn a_new_key_is_rotated_in_and_the_old_one_dropped() {
    let mut f = fixture();
    activity(&f);
    let before = view(&f);
    let parked = park(&mut f);
    let old = identity(f.config.path());
    let new = age::x25519::Identity::generate();
    let both = f.config.path().join("rotating.key");
    write_identities(&both, &[&new, &old]);
    let rotating = StateKey::load(&both).unwrap();
    assert_eq!(rotating.recipient(), new.to_public().to_string());
    // Both keys open everything as it is; the migration seals it all to the new one.
    let host = open_with(&f, &rotating, Migration::None).expect("old files open");
    drop(host);
    let host = open_with(&f, &rotating, Migration::Reseal).expect("the rotation");
    drop(host);
    let only_new = f.config.path().join("new.key");
    write_identity(&only_new, &new);
    let host = open_with(&f, &StateKey::load(&only_new).unwrap(), Migration::None)
        .expect("the new key alone");
    install(&mut f, host);
    drop(parked);
    assert_eq!(view(&f), before);
    assert_sealed(f.state.path());
    // The old key alone no longer opens it.
    let parked = park(&mut f);
    let refused = refusal(open_with(&f, &key(f.config.path()), Migration::None));
    assert!(refused.contains("does not open"), "{refused}");
    drop(parked);
}

#[cfg(unix)]
#[test]
fn files_are_0600_and_directories_0700() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777;
    let check = |state: &Path| {
        let mut stack = vec![state.to_path_buf()];
        while let Some(dir) = stack.pop() {
            assert_eq!(mode(&dir), 0o700, "{}", dir.display());
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    assert_eq!(mode(&path), 0o600, "{}", path.display());
                }
            }
        }
    };
    let mut f = fixture();
    // As written, with nothing tightened after the fact: intray's own files included.
    activity(&f);
    check(f.state.path());
    // State from before modes were set is tightened by the next open.
    let loose = f.state.path().join("talk/graphs/bobby");
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).unwrap();
    let feed = f.state.path().join("talk/feed.log");
    std::fs::set_permissions(&feed, std::fs::Permissions::from_mode(0o644)).unwrap();
    f.reopen();
    check(f.state.path());
}

/// The names of the files directly in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// Ledger #2017: a rotation re-encodes each pending tuple through intray (a new id, since an id
/// is the hash of the stored bytes) and the index follows it, keeping its seq; a tuple in a
/// settled stage moves to its new id with its note, rather than being sealed in place under a
/// name it no longer hashes to.
#[test]
fn a_reseal_moves_tuples_to_new_ids_and_keeps_their_seqs() {
    let mut f = matching_fixture();
    out(&f.kernel, &gonk(), &offer_named("a")).unwrap();
    out(&f.kernel, &gonk(), &offer("b")).unwrap();
    let space = f.state.path().join(TASKS).join("tuples");
    let inbox = space.join("inbox");
    let old_ids = names(&inbox);
    assert_eq!(old_ids.len(), 2);
    let reader = tasker(&participant("bobby"));
    let listed = source(&f.kernel, TUPLES, &[], &reader);
    // A dead letter and a settled answer, as a reactor over this space would leave them.
    let dead = sealed(f.config.path(), b"a dead letter");
    let dead_id = ikigai_intray::tuple_id(&dead);
    std::fs::write(space.join("error").join(format!("{dead_id}.tuple")), &dead).unwrap();
    std::fs::write(
        space.join("error").join(format!("{dead_id}.err")),
        "a note\n",
    )
    .unwrap();
    let done = sealed(f.config.path(), b"a settled tuple");
    let done_id = ikigai_intray::tuple_id(&done);
    std::fs::write(space.join("outbox").join(format!("{done_id}.tuple")), &done).unwrap();
    std::fs::write(
        space.join("outbox").join(format!("{done_id}.out")),
        sealed(f.config.path(), b"what the handler said"),
    )
    .unwrap();

    let parked = park(&mut f);
    let old = identity(f.config.path());
    let new = age::x25519::Identity::generate();
    let both = f.config.path().join("rotating.key");
    write_identities(&both, &[&new, &old]);
    let rotating = StateKey::load(&both).unwrap();
    let host = open_with(&f, &rotating, Migration::Reseal).expect("the rotation");
    install(&mut f, host);
    drop(parked);

    let new_ids = names(&inbox);
    assert_eq!(new_ids.len(), 2);
    assert!(new_ids.iter().all(|n| !old_ids.contains(n)), "{new_ids:?}");
    for name in &new_ids {
        let bytes = std::fs::read(inbox.join(name)).unwrap();
        assert_eq!(format!("{}.tuple", ikigai_intray::tuple_id(&bytes)), *name);
        age::decrypt(&new, &bytes).expect("sealed to the new key");
    }
    // The same tuples, the same seqs, in the same order.
    assert_eq!(source(&f.kernel, TUPLES, &[], &reader), listed);
    let ask = format!("ASK {{ ?o <http://purl.org/dc/terms/title> \"{TITLE}\" }}");
    let hits = source(&f.kernel, TUPLES, &[("match", &ask)], &reader);
    assert_eq!(hits.lines().count(), 1, "{hits}");
    assert!(hits.contains(&offer_iri("a")), "{hits}");
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();

    for (stage, old_id, note, plain) in [
        ("error", &dead_id, "err", b"a dead letter".as_slice()),
        ("outbox", &done_id, "out", b"a settled tuple".as_slice()),
    ] {
        let dir = space.join(stage);
        let left = names(&dir);
        assert_eq!(left.len(), 2, "{stage}: {left:?}");
        assert!(
            left.iter().all(|n| !n.starts_with(old_id.as_str())),
            "{left:?}"
        );
        let tuple = left.iter().find(|n| n.ends_with(".tuple")).unwrap();
        let bytes = std::fs::read(dir.join(tuple)).unwrap();
        let id = ikigai_intray::tuple_id(&bytes);
        assert_eq!(format!("{id}.tuple"), *tuple);
        assert_eq!(age::decrypt(&new, &bytes).unwrap(), plain);
        assert!(left.contains(&format!("{id}.{note}")), "{left:?}");
    }
    let answer = std::fs::read(
        space.join("outbox").join(
            names(&space.join("outbox"))
                .iter()
                .find(|n| n.ends_with(".out"))
                .unwrap(),
        ),
    )
    .unwrap();
    assert_eq!(
        age::decrypt(&new, &answer).unwrap(),
        b"what the handler said"
    );
}
