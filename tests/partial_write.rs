//! A write that fails part-way through a file it created (ledger #1221): nothing it began
//! may be left where the next open reads it as a record, and its sequence number is free
//! again, because nothing carrying it was stored.
//!
//! The fault is injected from outside, with no hook in the library: the process's file-size
//! limit (`RLIMIT_FSIZE`) is lowered below the size of the document being written, so the
//! write stops part-way with `EFBIG` (`SIGXFSZ` is ignored, so the process lives), exactly
//! the torn file a full disk or an I/O error leaves. The limit is the whole PROCESS's, so
//! every test in this file holds one lock for its whole run: no other test writes while the
//! limit is lowered.

#![cfg(unix)]

mod common;

use std::path::Path;
use std::sync::Mutex;

use common::*;
use ikigai_core::{Capability, Error, Kernel};
use ikigai_layers::id;

static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Run `during` with every regular-file write in this process limited to `bytes`.
fn with_file_size_limit<T>(bytes: u64, during: impl FnOnce() -> T) -> T {
    // SAFETY: plain libc calls on a local struct; the old limit is restored before return.
    unsafe {
        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
        let mut old = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        assert_eq!(libc::getrlimit(libc::RLIMIT_FSIZE, &mut old), 0);
        let low = libc::rlimit {
            rlim_cur: bytes as libc::rlim_t,
            rlim_max: old.rlim_max,
        };
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &low), 0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(during));
        assert_eq!(libc::setrlimit(libc::RLIMIT_FSIZE, &old), 0);
        result.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    }
}

/// Every file under `dir`, recursively, relative to it.
fn files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path.strip_prefix(dir).unwrap().display().to_string());
            }
        }
    }
    out.sort();
    out
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

#[test]
fn a_say_whose_entry_write_failed_leaves_no_partial_entry() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = fixture();
    let bobby = participant("bobby");
    let one = say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    let graphs = f.state.path().join("talk/graphs");
    let before = files(&graphs);

    let failed = with_file_size_limit(64, || say(&f.kernel, "talk", &bobby, &message("two")));
    assert!(
        matches!(failed, Err(Error::Endpoint(ref m)) if m.contains("write")),
        "{failed:?}"
    );
    assert_eq!(
        files(&graphs),
        before,
        "nothing of the failed entry is left"
    );

    // Nothing carrying its number was stored, so the next entry takes it.
    let two = say(&f.kernel, "talk", &bobby, &message("two again")).unwrap();
    assert_eq!(seq_of_entry(&two), 2);
    f.reopen();
    assert_eq!(
        events(&f.kernel, "talk"),
        vec![(1, "say".to_string(), one), (2, "say".to_string(), two)]
    );
}

#[test]
fn a_take_whose_receipt_could_not_be_staged_leaves_nothing_staged() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = fixture();
    let plasma = gonk();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    let receipts = f.state.path().join(TASKS).join("receipts");

    let failed = with_file_size_limit(64, || take(&f.kernel, &bobby_gonk(), &offer_iri("a")));
    assert!(matches!(failed, Err(Error::Endpoint(_))), "{failed:?}");
    assert_eq!(
        files(&receipts),
        Vec::<String>::new(),
        "nothing staged is left"
    );

    // The take did not happen: the tuple is still pending, and taking it works.
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    f.reopen();
    assert_eq!(
        events(&f.kernel, TASKS),
        vec![
            (1, "out".to_string(), offer_iri("a")),
            (2, "taken".to_string(), offer_iri("a")),
        ]
    );
}

/// The crash (or the failed removal) that leaves an entry write's `.partial` file behind:
/// the open clears it and never reads it as an entry.
#[test]
fn an_entry_write_left_partial_by_a_crash_is_cleared_on_open() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = fixture();
    let bobby = participant("bobby");
    let one = say(&f.kernel, "talk", &bobby, &message("one")).unwrap();
    let graphs = f.state.path().join("talk/graphs");
    let torn = graphs
        .join("bobby")
        .join(format!(".{}.ttl.partial", id::mint(1_791_590_499_000, 2)));
    std::fs::write(
        &torn,
        "@prefix ik: <https://ikigai-rs.dev/ns#> .\n<urn:layer:talk:en",
    )
    .unwrap();
    f.reopen();
    assert!(!torn.exists(), "cleared on open");
    let two = say(&f.kernel, "talk", &bobby, &message("two")).unwrap();
    assert_eq!(seq_of_entry(&two), 2);
    assert_eq!(
        events(&f.kernel, "talk"),
        vec![(1, "say".to_string(), one), (2, "say".to_string(), two)]
    );
}

/// The crash (or the failed removal) that leaves a torn staged receipt behind, for a tuple
/// that a later take did take: the open discards it rather than committing a receipt it
/// cannot read.
#[test]
fn a_torn_staged_receipt_is_discarded_on_open() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = fixture();
    let plasma = gonk();
    out(&f.kernel, &plasma, &offer("a")).unwrap();
    take(&f.kernel, &bobby_gonk(), &offer_iri("a")).unwrap();
    let staging = f.state.path().join(TASKS).join("receipts/.taking");
    let torn = staging.join(format!(
        "{}.{}.ttl",
        "0".repeat(64),
        id::mint(1_791_590_499_000, 2)
    ));
    std::fs::write(
        &torn,
        "@prefix prov: <http://www.w3.org/ns/prov#> .\n<urn:la",
    )
    .unwrap();
    f.reopen();
    assert!(!torn.exists(), "discarded on open");
    assert_eq!(
        files(&f.state.path().join(TASKS).join("receipts")).len(),
        1,
        "the take's own receipt, and nothing else"
    );
    assert_eq!(
        events(&f.kernel, TASKS),
        vec![
            (1, "out".to_string(), offer_iri("a")),
            (2, "taken".to_string(), offer_iri("a")),
        ]
    );
}
