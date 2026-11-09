# Accessibility

What `ikigai-layers` emits that people read, the rules that output follows, how it is tested, and
how to report a barrier. It is not a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text, RDF and JSON only (no HTML, SVG or color) |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-layers/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-layers/security/advisories/new) |
| Last reviewed | 2026-10-31 |

## What it emits that people read

This is a layer host library and one binary. A host's door (QUIC, an HTTP front, MCP) carries
the library's answers to participants; the door and any page it serves are the host's, not this
repository's.

- **Answers from the layer resources** (`src/endpoints.rs`): a write (`say`, `out`) answers the
  new entry's or tuple's IRI as `text/plain`; the feeds, the pending tuples and the receipts list
  one line per item as `text/plain`, space-separated (`<seq> <time> <kind> <iri> <participant>`
  for a feed event); entries, definitions and shapes are Turtle; and every answer a participant
  reads also comes as JSON or JSON-LD (README, "JSON faces").
- **Feed answers** (`src/feeds/`): a feed layer's entries also come as Atom 1.0 XML
  (`as=application/atom+xml`), written from the SELECT's rows by a stylesheet (a missing title
  is written `(untitled)`), and its graph and scheme as JSON-LD and JSON.
- **Refusals** (`src/endpoints.rs`, `src/channel.rs`, `src/tuplespace.rs`, `src/onboard/`):
  sentences that name the layer and the argument, such as ``cursor 9 is past layer `talk`'s last
  event, 4: … resynchronize from 0`` or ``tuple= names one tuple and match= selects by a
  template: give one``. An entitlement naming a participant another certificate holds answers
  ``onb:principal <participant> is already held by certificate <fingerprint>, …`` and says how
  to free it, on the onboarding status and the stderr line below. One naming a participant a
  manifest grants answers ``onb:principal <participant> is granted by manifest `<name>`, …`` and
  says how to change that, and one naming `onboard.controller` says it is never entitled, both
  in the same two places. A write that fails admission answers ``does not conform to layer
  `talk`'s shapes (N violation(s)): …``, listing up to 20 violations and a count of the rest.
  A `say` on a feed by a non-publisher answers ``layer `releases` is a feed, written only by
  its publishers, and <participant> is not one of them: ask in a Talk channel instead``; an
  entry whose `dcterms:subject` is outside the feed's scheme, and a `concept=` the scheme does
  not hold, each name the concept. A public join (`src/onboard/join.rs`) answers
  ``entitlement <iri>``, ``principal <iri>``, ``expires <time>`` and one ``granted <capability>``
  line each as `text/plain`, and the same as JSON. It refuses in sentences naming what it is
  about: a layer outside the public set (``layer `<name>` is not a public layer of this host: a
  join grants read and feed on public layers only, so nothing in this request was granted``),
  a limit (``join lobby `<name>` takes at most 5 requests an hour from one certificate, and
  <fingerprint> has made them; try again later``), the deny list, a certificate that has already
  joined, and an identity that has lapsed (which says it may join again). No answer, refusal or
  diagnostic names a source address, and a door whose system cannot supply randomness gets the
  error ``the hourly key for source addresses cannot be drawn: …`` from `door_capability`.
- **Host diagnostics on stderr** (`src/onboard/apply.rs`, `src/feeds/mod.rs`): lines starting
  `ikigai-layers: onboard:`, one per onboarding verdict, and one when applying entitlements
  fails or needs the door restarted; one starting `ikigai-layers: join:` when recording a
  joined identity's read fails; and one starting ``ikigai-layers: feed `<layer>`:`` when
  warming a feed's shared copy after a publish fails (the entry stands, and the first reader
  computes the answer).
- **The `ikigai-layers` binary** (`src/bin/ikigai-layers.rs`): usage text; `grants` prints one
  line per grant, client, unenrolled participant and idle certificate, then what it wrote;
  `onboard setup` prints what it did and a `stuck: …` line for each way a node would be left in
  the lobby; `descriptor` prints `wrote <path> and <path>`. Failures go to stderr.

Not this repository's: the text participants write into entries and tuples, which is stored and
answered as written; and how a door, a client or a terminal presents any of this.

## The rules that output follows

Verified by reading the source:

- A refusal is a sentence that names the layer, the argument or the participant it is about and
  says why; many say what to do instead (resynchronize from 0, give one of two arguments, pass a
  named flag).
- Every answer a participant reads has a JSON face, so a client can present it its own way
  without an RDF parser.
- The binary's lines start with a word that says what the line is (`grant`, `client`,
  `unenrolled`, `idle`, `stuck:`, `checked`, `wrote`), and a join's plain answer the same
  (`entitlement`, `principal`, `expires`, `granted`); `--check` ends by saying nothing was
  written; `-h`/`--help` prints the usage on stdout; a usage error exits 2 and a failure exits 1,
  each with its message on stderr.
- No color, no terminal control sequences and no markup are written.

## How it is tested

- `tests/grants.rs`: `the_binary_writes_both_files_or_nothing` runs the binary and checks that a
  refused configuration writes nothing and names the offending layer on stderr.
- `tests/setup.rs`: `setup_refuses_without_writing_anything` pins exit code 2 and the sentence
  naming `--door-dir`; the `check_finds_…` tests and
  `check_on_a_bare_config_names_every_missing_piece` pin exit code 1, each `stuck` reason and
  `nothing written (--check)`.
- `tests/channel.rs`, `tests/tuplespace.rs` and `tests/door.rs` check that a write outside the
  shapes is refused with `does not conform`; `tests/feed.rs`
  (`after_is_required_and_a_cursor_from_the_future_is_refused`), `tests/onboard.rs`
  (`an_entitlement_naming_a_hand_enrolled_participant_is_refused`,
  `an_entitlement_naming_a_participant_entitled_under_another_certificate_is_refused`,
  `an_entitlement_naming_a_participant_a_manifest_grants_and_no_certificate_holds_is_refused`,
  `an_entitlement_naming_the_uncertified_controller_is_refused`),
  `tests/me.rs`
  (`no_principal_is_a_refusal`, `an_unknown_face_is_refused`), `tests/feed_layers.rs`
  (`a_non_publisher_with_a_say_grant_is_refused_naming_the_feed`,
  `an_entry_subject_outside_the_scheme_is_refused_naming_it`,
  `an_unknown_concept_is_refused_naming_it`) and `tests/door.rs`
  (`a_second_host_over_one_state_directory_is_refused_naming_the_first`) pin other refusals.
- `tests/join.rs` pins the public join's refusals and answers
  (`a_request_naming_a_non_public_layer_gets_nothing`, `the_plain_answer_labels_each_line`,
  `a_denied_fingerprint_is_refused_at_join`, `the_per_certificate_limit_survives_a_restart`,
  `the_per_address_limit_refuses_the_eleventh_attempt_within_the_hour`,
  `an_expired_identity_is_refused_and_can_join_again`,
  `the_descriptor_refuses_a_bad_address_and_a_host_with_nothing_to_offer`).

These test wording and exit codes for correctness; none was written as an accessibility test.
**No automated accessibility checker, browser or assistive technology has been used on this
output.**

## Known limitations

Found by reading the source on 2026-10-31:

- **Unlabeled columns.** The plain feed, tuple and receipt listings are space-separated values
  with no header and no labels, so what each value is depends on its position.
- **Admission refusals are technical and long.** Each violation is the property's full IRI, the
  SHACL constraint component's name (`MinCountConstraintComponent`) and the validator's message,
  up to 20 of them joined by `;` on one line; `(node)` stands in when a violation has no path.
- **`grants` does not say which argument was wrong.** An unknown flag, a stray argument or a
  missing value answers with the usage text alone (`onboard setup` names an unknown flag).
- **Long opaque strings**: certificate fingerprints in `client <fingerprint> -> …` lines, and
  entry and tuple IRIs in every write answer and listing.
- **Parenthesized plurals** (`violation(s)`, `way(s)`), read with the parentheses, and one
  stderr line that marks urgency by capitals alone (`RESTART THE DOOR`).
- Every message is English only.

## Contributing

Read this file before changing any output listed above: a write answer, a listing's plain face, a
refusal, an onboarding diagnostic or the binary's text. Keep meaning in words, name what a
refusal is about, and keep a JSON face for anything a participant reads. Say in the pull request
what was tested, with what, and what was not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-layers/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your client, terminal and any
assistive technology, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-layers/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-08 | The refusal of an entitlement naming a participant another certificate holds. |
| 2026-11-08 | Feed layers: their refusals, the Atom face and the warm-failure stderr line. |
| 2026-11-08 | The public join: its answer, its refusals, its stderr line, and `descriptor`. |
| 2026-11-08 | The error when the join's hourly address key cannot be drawn; no output names an address. |
