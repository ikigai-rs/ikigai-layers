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
| Last reviewed | 2026-11-12 |

## What it emits that people read

This is a layer host library and one binary. A host's door (QUIC, an HTTP front, MCP) carries
the library's answers to participants; the door and any page it serves are the host's, not this
repository's. Documentation is Markdown, read on GitHub or in an editor; its rendered
presentation has not been evaluated here.

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
  <fingerprint> has made them; try again later``; the per-address one says ``from one source
  address, counting an IPv6 /64 network as one address``, with the host's `join.ipv6_prefix`),
  the deny list, a certificate that has already
  joined, and an identity that has lapsed (which says it may join again). No answer, refusal or
  diagnostic names a source address, and a door whose system cannot supply randomness gets the
  error ``the hourly key for source addresses cannot be drawn: …`` from `door_capability`.
- **The state key's refusals** (`src/at_rest.rs`): ``the state identity <path>: …`` says what
  is wrong (does not exist, cannot be read, a bad line by its number, or, on unix, an owner or
  mode another account could use) and, for an owner or a mode, the `chmod` or `chown` to run,
  with the path quoted when it needs to be. No refusal echoes a line of the file.
- **The operator's join count** (`src/onboard/join.rs`, `urn:layers:join:count`): three labeled
  lines as `text/plain` (``joined <count>``, ``read-in-7-days <count>``, ``read-in-30-days
  <count>``), and the same as JSON, where a count under 5 reads `fewer than 5`.
- **Host diagnostics on stderr** (`src/onboard/apply.rs`, `src/feeds/mod.rs`): lines starting
  `ikigai-layers: onboard:`, one per onboarding verdict, and one when applying entitlements
  fails or needs the door restarted; one starting `ikigai-layers: join:` when recording a
  joined identity's read fails; and one starting ``ikigai-layers: feed `<layer>`:`` when
  warming a feed's shared copy after a publish fails (the entry stands, and the first reader
  computes the answer).
- **The `ikigai-layers` binary** (`src/bin/ikigai-layers.rs`): usage text; `grants` prints one
  line per grant, client, unenrolled participant and idle certificate, then what it wrote,
  and refuses a configuration directory that is missing, not a directory or holds no manifest,
  saying which and that nothing was written;
  `onboard setup` prints what it did and a `stuck: …` line for each way a node would be left in
  the lobby; `descriptor` prints `wrote <path> and <path>`. Failures go to stderr.

Not this repository's: the text participants write into entries and tuples, which is stored and
answered as written; and how a door, a client or a terminal presents any of this.

## The rules that output follows

These rules describe the intended output. The results below distinguish the checks run from
limitations and checks that could not run:

- A refusal is a sentence that names the layer, the argument or the participant it is about and
  says why; many say what to do instead (resynchronize from 0, give one of two arguments, pass a
  named flag).
- Every answer a participant reads has a JSON face, so a client can present it its own way
  without an RDF parser.
- The binary's lines start with a word that says what the line is (`grant`, `client`,
  `unenrolled`, `idle`, `stuck:`, `checked`, `wrote`, `unchanged`, `kept`, `differs`, `backup`), and a join's plain answer the same
  (`entitlement`, `principal`, `expires`, `granted`) and the join count's (`joined`,
  `read-in-7-days`, `read-in-30-days`); `--check` ends by saying nothing was
  written; `-h`/`--help` prints the usage on stdout; a usage error exits 2 and a failure exits 1,
  each with its message on stderr.
- The binary writes plain terminal text without color or terminal control sequences. Resource
  answers also include structured formats such as Turtle and Atom XML.

## How it is tested

Every named check has a result: Pass, Fail or Blocked, with evidence. A Pass covers only the
stated check, not overall accessibility or WCAG conformance. Completed checks below were run on
2026-11-12 against main at `92e37d77603f132a77793ab98f6e159f243fa913`, before this documentation
change. The result format follows [gonk-starter PR 91](https://github.com/ikigai-rs/ikigai-gonk-starter/pull/91).
The Rust tests check wording, data and exit codes for correctness; none was written
as an accessibility test. CI repeats the Rust tests, but not the manual source review or
assistive technology checks.

The test command was `cargo test --test grants --test setup --test channel --test tuplespace
--test door --test feed --test onboard --test me --test feed_layers --test join --test at_rest
--test json`, using Cargo 1.99.0 and rustc 1.99.0: all 230 tests passed. Each test-file row below identifies the part
of that run it covers. Source-review rows use Git 2.43.0's `git show` against the main commit
above; they describe code inspection, not a user evaluation.

| Check | Result | Evidence |
|---|---|---|
| Enrollment binary refusals and no-write behavior | Pass | `tests/grants.rs`: `the_binary_writes_both_files_or_nothing` and `a_missing_or_empty_config_directory_is_refused_and_nothing_is_written` check stderr naming the layer or directory and that refusal writes nothing. Does not evaluate terminal presentation. |
| Setup diagnostics and exit codes | Pass | `tests/setup.rs`: `setup_refuses_without_writing_anything`, the `check_finds_…` tests and `check_on_a_bare_config_names_every_missing_piece` check the named argument, exit codes 2 or 1, `stuck` reasons and no-write text. Does not test comprehension. |
| Admission and host refusals | Pass | `tests/channel.rs`, `tests/tuplespace.rs` and `tests/door.rs` check `does not conform`; the door suite also checks `a_second_host_over_one_state_directory_is_refused_naming_the_first`. Wording assertions do not establish that long SHACL messages are usable. |
| Feed cursor refusal | Pass | `tests/feed.rs`: `after_is_required_and_a_cursor_from_the_future_is_refused` checks the required argument and future cursor refusal; no assistive technology used. |
| Onboarding identity refusals | Pass | `tests/onboard.rs`: the four `an_entitlement_naming_…_is_refused` tests cover a hand-enrolled participant, another certificate's participant, a manifest-granted participant and the uncertified controller. Checks refusal content, not how a door presents it. |
| Participant identity refusals | Pass | `tests/me.rs`: `no_principal_is_a_refusal` and `an_unknown_face_is_refused` check refusal text, not client presentation. |
| Feed publisher and concept refusals; Atom and JSON faces | Pass | `tests/feed_layers.rs`: `a_non_publisher_with_a_say_grant_is_refused_naming_the_feed`, `an_entry_subject_outside_the_scheme_is_refused_naming_it`, `an_unknown_concept_is_refused_naming_it`, and the suite's representation tests. Checks content and formats, not a rendered feed reader. |
| Public join answers, refusals and counts | Pass | `tests/join.rs`: covers non-public layers, denied fingerprints, certificate and address limits (including IPv6 /64), expired identities, bad descriptors, labeled plain answers, three count fields, counts under five and unknown count faces. Does not evaluate reading or navigating the output with assistive technology. |
| State key refusal guidance | Pass | `tests/at_rest.rs`: `a_key_another_account_can_read_or_write_is_refused_naming_the_chmod`, `a_key_in_a_directory_another_account_can_write_is_refused_naming_the_chmod`, `a_path_with_a_space_is_quoted_in_the_chmod`. Checks command text and quoting on Unix, not user comprehension. |
| Machine-readable alternatives | Pass | `tests/json.rs`: write answers, entry and graph JSON-LD, tuple and receipt JSON, unknown faces and `every_json_face_is_declared`; join and feed-layer suites cover their own faces. These are representation checks, not an evaluation of any client's accessible presentation. |
| Binary line labels, help streams and exit routing | Pass | `git show 92e37d7:src/bin/ikigai-layers.rs`: inspected `run`, `main`, `grants` and `onboard_setup` for labels, help on stdout, errors on stderr and exit codes. Source review only; no manual terminal usability pass. |
| Binary color and terminal control sequences | Pass | `git show 92e37d7:src/bin/ikigai-layers.rs`: inspected output calls and string literals; no color or terminal control sequences added by the binary. Does not cover arbitrary supplied paths or a terminal's rendering. |
| Manual keyboard and terminal navigation | Blocked | No person performed a keyboard or terminal navigation pass for this review; no interactive user evaluation session was available. |
| Browser rendering of documentation or host output | Blocked | No browser evaluation was run. This repository does not supply the host's door or client UI; documentation as rendered on GitHub also remains unevaluated. |
| Automated accessibility checker (such as axe) | Blocked | No automated accessibility checker was run; no rendered client or documentation checker was configured for this review. Rust tests are not a substitute. |
| Screen reader | Blocked | No screen reader was used on this output; no screen reader session was available for this review. |
| Screen magnifier | Blocked | No screen magnifier was used on this output; no magnifier session was available for this review. |
| Speech input | Blocked | No speech-input software was used on this output; no speech-input session was available for this review. |
| Switch access | Blocked | No switch-access software was used on this output; no switch-access session was available for this review. |
| Braille display | Blocked | No braille display was used on this output; no braille display was available for this review. |

## Known limitations

Rechecked by reading main's source on 2026-11-12 with `git show` (Git 2.43.0), not by testing
with users. Fail records a remaining output limitation, not a completed WCAG assessment.
These source reviews are dated evidence; CI does not repeat them.

| Check | Result | Evidence |
|---|---|---|
| Plain listing column labels | Fail | `git show 92e37d7:src/endpoints.rs`: feed, tuple and receipt plain listings use space-separated values without headers or field labels; meaning depends on position. |
| Admission refusal readability | Fail | `git show 92e37d7:src/endpoints.rs`: admission messages use full property IRIs, constraint component names and validator messages, up to 20 violations joined by semicolons on one line; `(node)` substitutes for a missing path. |
| Naming the invalid grants argument | Fail | `git show 92e37d7:src/bin/ikigai-layers.rs`: `grants` returns usage alone for an unknown flag, stray argument or missing value. `onboard setup` names an unknown flag. |
| Readability of opaque identifiers | Fail | `git show 92e37d7:src/bin/ikigai-layers.rs` and `git show 92e37d7:src/endpoints.rs`: client lines use full certificate fingerprints; write answers and listings use full entry and tuple IRIs. |
| Natural plurals and urgency wording | Fail | `git show 92e37d7:src/endpoints.rs`, `git show 92e37d7:src/bin/ikigai-layers.rs` and `git show 92e37d7:src/onboard/apply.rs`: `violation(s)`, `way(s)` and `RESTART THE DOOR` remain in messages. Spoken presentation has not been tested. |
| Message localization | Fail | Source review of `src/bin/ikigai-layers.rs`, `src/endpoints.rs` and `src/onboard/` at the main commit above: built-in messages are English only, with no localized alternatives. |

## Contributing

Read this file before changing any output listed above: a write answer, a listing's plain face, a
refusal, an onboarding diagnostic or the binary's text. Keep meaning in words, name what a
refusal is about, and keep a JSON face for anything a participant reads. Say in the pull request
what was tested in a Check | Result | Evidence table. Give each check Pass, Fail or Blocked,
with its test or command and tool version, or why it could not run. Keep unrun checks Blocked,
including assistive technologies nobody used. A historical run CI does not repeat is Blocked
with its date until rerun; it is not a current Pass.

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
| 2026-11-08 | The operator's join count: labeled lines and a JSON face. |
| 2026-11-08 | The per-address join refusal says an IPv6 network counts as one address. |
| 2026-11-09 | The state key's refusals, with the owner and mode ones naming the command to run. |
| 2026-11-12 | Record checks as Pass, Fail or Blocked, with current main evidence and explicit evaluation gaps. |
