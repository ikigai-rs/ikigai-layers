# The first coordinated security response (2026-10-21)

On 2026-10-21 two security problems reached the ecosystem on the same evening, and for the first time the people
and agents working on ikigai answered one together, over the layer `talk`. The operator's side found and fixed what
was ours, told the collaborator's side what to do and what was not affected, and the collaborator's side acted and
reported back, all in the same few hours. This note is a placeholder for a fuller write-up: it records what happened,
in what order, and what each side did, while it is fresh.

## What happened

**Recoverable CA keys.** Eight intermediate certificate authorities operated by eMudhra had been generated so that their
private keys could be recovered from the public certificates, possible since April 2025
(https://emudhra.we-have-a-problem.com/). They chain to roots that Mozilla, Microsoft, Apple and Google trust, so
anyone could have minted a certificate for any site. Browsers blocked them between 17 and 20 October; everything else
needed an update.

For Rust code that meant **RUSTSEC-2026-0382 (CVE-2026-111943)**: rustls-webpki below 0.103.16 may accept a certificate
path through those intermediates. The fixed versions were rustls-webpki 0.103.16 and webpki-roots / webpki-root-certs
1.0.10, with rustup 1.29.2 for the toolchain installer.

The same day brought unrelated advisories for PHP ("PHP2Shell", CVE-2026-109618 and CVE-2026-109619, exploited before
disclosure) and Node.js. ikigai uses neither as a server: Node appears only as build and test tooling.

**What was exposed, and what was not.** Every ikigai-rs repository that commits a `Cargo.lock` was locked to
rustls-webpki 0.103.15, and so was every published ikigai-cli up to 0.1.67. The exposure was outbound TLS to the public
web: HTTP fetches, hosted model clients, mail. **The QUIC doors, including this layer door, were never exposed**: both
sides trust only pinned, self-issued certificates, so nothing chaining to eMudhra could stand in for either one.

## The response, in order (UTC)

1. **22:50, 21 Oct.** The hub posted the advisory on `talk` for the collaborator's side
   ([`talk-entry-the-advisory.ttl`](talk-entry-the-advisory.ttl), entry 57). It said what happened, what to update on
   their machines (rustup, the three crates in any committed lock, the OS trust stores, Node, the JDK, Claude Code,
   Python's certifi), what was not affected, and asked for errors back as text.
2. **22:51-23:45.** Six lock-only pull requests moved every committed lock to the fixed crates. Each PR carried a
   per-package diff of the lock, so the PR history states exactly what moved. The ikigai-cli and ikigai-gonk ones went
   last, after two in-flight pin updates had merged, so the locks did not conflict.
3. **23:46.** The hub posted on `talk` that every lock had moved, and that binaries should be rebuilt from main with
   `--locked` ([`talk-entry-rebuild-from-main.ttl`](talk-entry-rebuild-from-main.ttl), entry 58).
4. **Overnight, on the operator's side:** the binaries were rebuilt from main and their services restarted on every
   machine that runs them; the Linux machines took the fixed `ca-certificates`; the web and mail server's PHP was
   confirmed already at the fixed 8.4.27 (and not loaded by its web server), and a test message through its mail server
   came back with SPF, DKIM and DMARC all passing; the command-line Claude Code moved past the fixed 2.1.303.
5. **03:13, 22 Oct.** ikigai-cli 0.1.68 was published: the first release whose own lock carries the fix, so a plain
   `cargo install ikigai-cli --locked` is safe again. Its lock moved only the workspace's own versions (a re-resolve
   would also have moved unrelated third-party edges, so the lock was edited by hand and checked with
   `cargo metadata --locked`).
6. **03:17.** The hub posted the release on `talk`, asking anyone on an earlier version to reinstall
   ([`talk-entry-release-0.1.68.ttl`](talk-entry-release-0.1.68.ttl), entry 59).
7. **05:27.** The collaborator's agent, `urn:collab:participant:bobby-claude`, reported what their side had done
   ([`talk-entry-collaborator-status.ttl`](talk-entry-collaborator-status.ttl), entry 60):
   - rustup 1.29.2 and Claude Code 2.1.304;
   - certifi updated in their layers client's environment;
   - ikigai and ikigai-gonk rebuilt from main with `--locked`, and gonk restarted on it;
   - their own Slack door, a separate Rust binary, moving its lock to 0.103.16;
   - their ikigai-cli fork rebasing onto main before its next upstream pull request;
   - Windows Update done. Their Linux distribution's `ca-certificates` had no update yet, so they distrusted the
     affected emSign roots locally until it does. Node was next.

One machine's JDK remains for the next day; the tracking item records it.

## What made it work

- **The layer was the channel.** Both sides already shared `talk`, with identities enrolled per actor, so the
  advisory reached the collaborator's agent directly. The agent read it, acted, and answered on the same layer.
- **Said what was NOT affected.** Naming the unexposed part (the pinned QUIC doors) up front kept both sides from
  spending the evening on it.
- **Asked for errors as text, ran nothing on the other side.** Each side updated its own machines. Nothing in any
  message asked the other side to run code on ours, or ours on theirs.
- **Evidence in the record.** Every lock change was a pull request with its diff; the release was tagged at the exact
  commit published; the operator's work is tracked as one ledger item with a comment per step.

Nothing here is read by the crate or its tests; it is history, not a fixture.
