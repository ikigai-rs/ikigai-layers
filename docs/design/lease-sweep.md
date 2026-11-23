# Design spike: lease maintenance as a scheduled sweep

**Status:** a design for a decision, not a decision. Nothing here is built. This document
changes no host code, client, shape or configuration (ledger #4074). Brian decides before any
implementation item is filed.

**The question.** A tuplespace layer defined with `lease_max` gives a `take lease=` a deadline.
Today the host returns every lapsed lease before every request on the layer, reads included,
so a Source at a lapse appends a `returned` event to `feed.log`: a read writes (ledger #3985,
pinned by `tests/lease.rs:363`). The action-consistency note (ikigai-core PR 335,
`docs/design/action-consistency.md`, ledger #2771) wants Source to have no side effects, so a
read can be cached, replayed and reordered. This note compares ways to keep leases lapsing on
time without a read ever writing, and recommends one.

**How facts are cited.** `file:line` refers to this repository at `d48cec7` unless another
repository is named. Other repositories are cited at these commits:

| repository | commit |
| --- | --- |
| [ikigai-ledger](https://github.com/ikigai-rs/ikigai-ledger) | `01467281bc1724dfd0abe7dcd68a20251186a46d` |
| [ikigai-layers-client](https://github.com/ikigai-rs/ikigai-layers-client) | `d0ad6d9a97d4e4d4c92234c32fd425b49a3bdb98` |

A claim marked **(unverified)** was not checked against source.

## Summary

- **Recommendation: (c), lazy evaluation, with a scheduled sweep as the writer for idle
  layers.** A read computes a lease as lapsed from `until <= now` and never writes. The
  `returned` event is written by whichever of these runs first after the lapse, always under
  the layer's write lock and always as `urn:layers:lease`: the host's sweep, or the next write
  on the layer (`out`, `take`, a settle).
- **The lapse instant is the lease's `until`**, not the moment someone noticed. The `returned`
  event's `time` becomes `until`, so the feed is a function of the writes and the leases, not
  of who read when.
- **Reads on a leased layer become cacheable** until the earliest `until` in the answer
  (`Representation::cacheable_until`), hanging from the tuples thread as today. The lapse is a
  scheduled expiry, not a cut. The sweep's write cuts the feed thread.
- **Release impact:** a documented behavior changes (README "lease", `README.md:681`), so it
  is a minor release under 0.x: 0.12.0. No known client depends on a read writing.

## Today, measured

The host runs the lease sweep in the endpoint's pre-dispatch, before any action
(`src/endpoints.rs:2344`), for every tuplespace layer whose kernel has a clock. The sweep
(`src/host.rs:117`) scans `due` (`src/tuplespace.rs:407`) without the lock, and if anything is
due takes the layer's write lock, scans again, then for each lapse stores the lease file with
its feed number, drops the tuple back and logs `returned` (`src/host.rs:132`). The retention
sweep runs the same way, before every request (`src/endpoints.rs:2329`).

Two measurements, made with throwaway tests over `tests/lease.rs`'s `ManualClock` fixture
(not committed):

1. **Whether a read writes depends on the kernel's cache.** A definition read at minute 9,
   then the clock moved to minute 10 on the same kernel, then a second definition read: the
   second read left `feed.log` unchanged (`cached-def-read wrote: false`), because the
   definition is `cacheable()` (`src/endpoints.rs:583`) and the kernel answered it without
   invoking the endpoint. The same read on a fresh kernel at the same instant wrote
   `returned` (`cold-def-read wrote: true`). So today the side effect is neither reliable nor
   absent: it depends on what is cached.
2. **The `returned` event's time is the sweep instant.** Lease `until` 00:10, no request until
   00:25: the feed reads `3 2026-10-10T00:25:00.000Z returned …`. The lease file records the
   same 00:25 as the lapse time (`src/host.rs:145`), while the late-settle refusal says
   `lease lapsed at <until>`, 00:10 (`src/endpoints.rs:1986`). The host already gives two
   different answers to "when did it lapse".

Also already true: a settle computes the lapse lazily. A `done` or `renew` on a `Held` lease
whose `until` has passed is refused without waiting for the sweep (`src/endpoints.rs:1996`).
A host open repairs a lapse whose tuple never went back (`src/tuplespace.rs:891`) but does not
sweep leases that lapsed while it was down (`src/host.rs:349` runs only retention), so after a
restart a lapsed tuple reads as taken until the first request.

## 1. Mechanisms

- **(a) Today: return before every request.** Every action, Source included, sweeps first.
- **(b) Host-scheduled sweep only.** A timer (one per layer, or one host clock) calls
  `Host::sweep_leases(now)`. It is the only writer of `returned`. Reads and writes show the
  stored state: a lapsed, unswept lease still reads as taken.
- **(c) Lazy evaluation without writing.** Every read COMPUTES a `Held` lease with
  `until <= now` as lapsed: the tuple reads as pending, the receipt as `lapsed`. Nothing is
  stored by a read. The lapse is recorded by the sweep or, if a write comes first, by that
  write, under the layer's write lock, before it does anything else.

| | (a) today | (b) sweep only | (c) lazy, recorded by sweep or next write |
| --- | --- | --- | --- |
| **Reader at the lapse** | sees the tuple back, if its request reached the endpoint (a cached read does not, measurement 1) | sees it taken until the sweep runs: stale for up to one sweep period | sees it back from `until` on, every time, cached or not |
| **Does a read write** | yes, sometimes | no | no |
| **Feed order** | depends on who read first; `returned` lands wherever the first request after the lapse is | depends on the sweep's schedule; an `out` at 00:12 before a 00:15 sweep logs `out` before the 00:10 `returned` | a write records every due lapse first, in `until` order, so `returned` precedes every later event; times in the feed never go backward |
| **`returned` time** | the first request's instant | the sweep's instant | the lease's `until` (recommended in section 3) |
| **Crash between stored lapse and tuple back** | repaired on open (`src/tuplespace.rs:891`), unchanged | same | same |
| **Restart after a lapse while down** | taken until the first request | taken until the first sweep | pending at once (computed); recorded by the sweep or the first write |
| **Cost per request** | a `due` scan of the layer's live receipts on every request (`src/tuplespace.rs:407`, linear) | none | a `passed` comparison per receipt a read already lists; a write pays the `due` scan it pays today |
| **Cost with many layers** | none when idle; each request pays for its own layer | one timer per layer, or one host loop polling every layer | as (b) for the idle case: one host-wide earliest-`until`, so one wakeup per lapse, not per layer |
| **Atomic take** | holds (section 2) | a lapsed, unswept tuple cannot be taken: fails "must be able to take one that has" unless take sweeps, which makes it (c) | holds (section 2) |

(b) alone fails the take requirement and gives stale reads, and fixing either one turns it
into (c). (a) fails the side-effect requirement. That leaves (c).

### How the sweep is scheduled under (c)

The library cannot own a timer: on wasm32 it has none (`src/timer.rs:4`). So the host exposes
when the next lapse is due and leaves the waiting to the deployment, as it already does for
`Host::sweep_retention` and `Host::sweep_leases` (`src/host.rs:368`):

- `Host::next_lapse() -> Option<Time>`: the earliest `until` of a `Held` lease across all
  layers, kept as a min-ordered index beside each layer's `live` set so it is O(log n) to
  maintain, not a scan.
- The deployment (the binary's door, gonk's host, the edge) sleeps until `next_lapse()` with
  its `Timer` and calls `sweep_leases(now)`, then asks again. A take or renew that makes an
  earlier `until` wakes it (the host's `Notify`, `src/host.rs:564`, already exists for this
  kind of signal).
- `Host::open` sweeps leases at its open instant, as it does retention, so a restart records
  what lapsed while it was down.

Under (c) the sweep's schedule affects only when the feed shows `returned`, never what a reader
or a taker sees, so a late or missed wakeup cannot make an answer wrong.

## 2. Atomic take

The requirement: a take never hands out a tuple whose lease has not lapsed, and can take one
whose lease has.

Every path that writes a lapse, and every take, holds the layer's write lock
(`self.state.write`, `src/endpoints.rs:1551`; `src/host.rs:131`), and all of them decide a
lapse with the same predicate, `tuplespace::passed(until, now)`, `until <= now`
(`src/tuplespace.rs:252`). Under (c) the take, inside the lock:

1. looks the subject up (`src/endpoints.rs:1553`);
2. if it is `Taken` by a receipt whose lease is `Held` and `passed(until, now)` is false,
   refuses as today (`already_taken`, `src/endpoints.rs:2235`): never handed out early;
3. if it is `Taken` by a receipt whose lease has passed, records the lapse first (lease file,
   tuple back, `returned` by `urn:layers:lease`), exactly as the sweep does today, then takes
   it as pending: the take of a lapsed tuple succeeds;
4. takes, with the receipt counting the attempt as today (`src/tuplespace.rs:341`).

**A take racing the sweep.** Both take the same lock, and the sweep already re-reads `due`
after taking it (`src/host.rs:132`). If the sweep wins, the take finds the tuple pending. If
the take wins, it records the lapse itself and takes; the sweep then finds the old receipt no
longer live (the new take supersedes it, `src/tuplespace.rs:359`) and does nothing. Either
order logs one `returned` and one `taken`, in that order. The same holds for two sweeps, and
for a settle racing the sweep: the settle refuses on `passed` whether or not the lapse is
recorded (`src/endpoints.rs:1996`).

**At the boundary instant.** `until == now` is lapsed for the reader, the taker, the settler
and the sweep alike, because they share `passed`. A kernel whose clock reads earlier sees the
lease held and refuses the take: it can be late, never early.

Under (a) the argument is the same (the pre-dispatch sweep holds the lock). Under (b) step 3
does not exist, so a lapsed tuple cannot be taken until the sweep runs.

## 3. Time

**Whose clock.** The kernel's injected `Clock`, as today (`inv.now()`, and the clock the
deployment passes to `sweep_leases`). Natively that is the host's `Monotonic`
(`src/native.rs:31`), which never runs backward; a test injects `ManualClock`
(`tests/lease.rs:16`). No change.

**What `returned`'s time means.** Two candidates:

- **The sweep instant** (today). It records when the host noticed, which depends on traffic
  or a timer. Two hosts replaying the same writes produce different feeds.
- **The lapse instant, the lease's `until`.** It is fixed when the lease is granted or
  renewed, so the feed depends only on the writes. It agrees with what a reader under (c)
  already saw (the tuple pending from `until` on) and with the late-settle refusal, which
  already says `lapsed at <until>`.

**Recommend the lapse instant.** The `returned` event's `time` and the lease file's lapse
`time` both become `until`. Under (c) feed times stay monotonic, because every write records
the due lapses (all with `until <= now`) before its own event, in `until` order. If an operator
wants to know how late the host noticed, the lease file can carry it as a separate
`recorded` field; it is not needed for correctness and is not proposed for the feed.

## 4. Cacheability

Today `rd` on a layer with `lease_max` (or `retain`) is never cacheable, because "a lapsed
lease advances without a Sink or Delete to cut the kernel's cache" (`src/endpoints.rs:1716`).
Under (c) it can be:

- **`rd` (pending or `state=taken`), a receipt, `rd tuple=`:** cacheable until the earliest
  `until` of a `Held` lease the answer depends on, `cacheable_until(until)`, and
  `depends_on(tuples_thread)` as today. The kernel cuts the tuples thread on every Sink and
  Delete through the `tuples` door (`src/endpoints.rs:1443`). A pending listing depends on every
  held lease in the layer (any could come back into it), so its deadline is the layer's
  earliest `until`; a single receipt's deadline is its own.
- **So the lapse cuts nothing; it is a scheduled expiry** at the lease's `until`. Recording the
  lapse later changes no tuple read (the computed answer was already the lapsed one), so the
  sweep does not need to cut the tuples thread.
- **The feed read** changes when `returned` is logged, which under (c) happens only in a write
  (the sweep or a client's write). That write cuts the feed's thread and bumps `Notify`, as
  writes do. A feed read is cacheable until the next write; it is not cacheable "until the
  lapse", because a feed shows what is recorded, and the recording time is not known in
  advance.
- **Definition, shapes, context:** already cacheable (`src/endpoints.rs:583`); they simply
  stop sweeping, which ends measurement 1's cache-dependent side effect.

A lease change (take, renew, done) is a Sink or Delete through the `tuples` door or the
receipt, so it cuts the thread and resets any deadline computed from the old `until`
(**(unverified)** that a Sink on a receipt IRI cuts the layer's tuples thread; if it does not,
the receipt endpoint must cut it, and the implementation item says so).

## 5. Capabilities

The sweep writes as the host, never as a participant:

- `returned` is attributed to `urn:layers:lease` (`LEASE_AGENT`, `src/tuplespace.rs:1292`), as
  retention's `sweep` is to `urn:layers:retention`. Unchanged.
- `Host::sweep_leases` is a library call the deployment makes on its own authority, over the
  host's own state; it goes through no participant door and needs no grant, as the private
  `urn:cap:space:*` authority the tuplespace already uses for intray is the host's own
  (`src/tuplespace.rs:4`). If it is later routed through the kernel, it gets a host-minted
  capability like that one, never a participant's.
- Under (c) a read never writes, so no reader's `read` grant ever covers a write.
- A write that records a due lapse (step 3 of section 2) does so under a write grant the
  caller already holds on the layer (`take` or `out`), and attributes the `returned` to
  `urn:layers:lease`, not to the caller. The write would have serialized behind that lapse
  anyway; the caller gains no ability it did not have. If Brian wants no write to perform
  maintenance at all, the alternative is that a write records nothing and the take treats a
  passed lease as pending in memory, leaving the record to the sweep; that loses the
  "`returned` precedes `taken`" feed order (a `taken` with attempt 2 could precede its
  `returned`), which is why it is not recommended.

## 6. Compatibility

**Observable changes:**

1. A read never appends to `feed.log` or changes a lease file. A client that tails a leased
   layer's feed waiting for `returned` sees it when the sweep or the next write runs, not when
   someone reads. A deployment that schedules no sweep sees `returned` only at the next write
   or open.
2. `returned`'s `time` is the lease's `until`, not the noticing instant.
3. After the lapse and before it is recorded, a receipt reads `lease=lapsed`, but its lapse
   has no feed number yet. The JSON receipt gains `"recorded": false` there (and `true` once
   recorded); the plain line keeps its fields (ACCESSIBILITY.md lists them) and adds
   `recorded=no` only in that window. This follows ikigai-ledger's stalled view, which says
   whether each lapse is recorded yet.
4. `rd` on a leased layer becomes cacheable (`cacheable_until`).

**Clients:**

- **The Python layers client** (ikigai-layers-client at `d0ad6d9`): a code search finds no use
  of `lease=`, `lapsed` or a `returned` event; its `returned` and `release` matches are
  unrelated (feed paging, offer releases in `work.py`). No change needed.
- **gonk's layers pages and the edge's layers:** a search of the ikigai-rs organization for
  `urn:layers:lease` finds only this repository. **(unverified)** that neither renders lease
  states from the receipt JSON; if one does, item 3's `recorded` field is additive.
- **Deployments** gain an obligation they already had on paper (README, "for an idle host, the
  deployment schedules `Host::sweep_leases(now)`"): schedule it, now with `next_lapse()`.

**Tests this implies** (`tests/lease.rs`):

- `a_definition_read_at_the_lapse_returns_the_tuple` (line 363) is inverted and renamed, as
  `a_read_at_the_lapse_writes_nothing`: at minute 10, a definition read, an `rd`, a receipt
  read and a feed read each leave `feed.log` and the lease file byte-identical; the `rd`
  already shows the tuple pending and the receipt `lapsed`, with `recorded` false.
- `a_lapsed_lease_returns_the_tuple_to_the_space` (line 102): its `kinds == [out, taken,
  returned]` after a read becomes `[out, taken]`, and `[out, taken, returned]` after
  `sweep_leases`. Its take again at minute 10 then logs `[…, returned, taken]` whether or not
  the sweep ran.
- New: `a_take_at_the_lapse_records_it_first`: no sweep, a take at minute 10 succeeds and the
  feed reads `out, taken, returned, taken`, with `returned`'s time the lapse instant.
- New: `the_returned_time_is_the_lapse_instant`: lease until 00:10, `sweep_leases` at 00:25,
  the event reads 00:10.
- New: `a_take_racing_the_sweep_returns_once`: a take and `sweep_leases` started together on
  a multi-threaded executor; exactly one `returned`, then one `taken`.
- New: `next_lapse_names_the_earliest_until` across two layers, moved by a renew.
- New: `open_records_a_lapse_while_down`: reopen at minute 15, feed has `returned` at 00:10.
- New: `a_leased_rd_is_cached_until_the_lapse`: two `rd` on one kernel at minutes 8 and 9 hit
  the cache; at minute 10 the kernel invokes again and the tuple is pending.
- Unchanged: `a_lapse_whose_tuple_never_went_back_is_repaired_on_open` (line 324), late
  settle, renew, attempts across a restart.

**Release impact.** The README contract written in PR 85 ("a read on a leased layer can
therefore grow the feed") is reversed, and `returned`'s time changes meaning. Under 0.x that
is a breaking change, so a minor release: **0.12.0**, with the README's lease section, the
pre-dispatch comment and ACCESSIBILITY.md's receipt line updated in the same pull request.
The wire format gains only the additive `recorded` field.

## 7. Ecosystem

The same pattern, maintenance done by a read, appears in these places; none is fixed here:

- **This repository, retention.** The retention sweep runs before every request, reads
  included (`src/endpoints.rs:2329`), and appends a `sweep` event (`src/host.rs:97`). Its
  cutoff is computed from `now`, so it has measurement 2's problem too. The same design
  applies: compute expiry on read, record it in the sweep or the next write.
- **This repository, the public join's idle limit.** Every request a joined certificate makes
  passes `Join::gate` (`src/onboard/join.rs:679`), which writes the identity's `last_read` day
  (line 701) and, past the idle limit, lapses the identity (line 690), so a read writes a
  record and can revoke. Recording a read is the point of that limit, so this one is a
  deliberate write; it is named here because it puts a write under a read grant.
- **ikigai-ledger claim leases: not this pattern.** The ledger chose (c) already
  ([`src/endpoints/lapse.rs`](https://github.com/ikigai-rs/ikigai-ledger/blob/01467281bc1724dfd0abe7dcd68a20251186a46d/src/endpoints/lapse.rs),
  ledger #3709): it rejects "a sweep on read" because `next`, `items` and the doctor are cached
  reads under the read grant, computes the stalled view from the claims as they stand, and
  records each lapse once through a `lapse` Sink that gonk's dispatcher tick sends under the
  dispatcher's grant. Its `doctor/next_expiry.rq` is the `next_lapse()` this note proposes.
  One difference: an expired ledger lease is never released, only recorded; a layer lease
  returns its tuple, which is why the take must record the lapse itself (section 2).

## Recommendation

Adopt (c): reads compute lapses and never write; the lapse instant is `until`; the sweep,
scheduled by the deployment from `Host::next_lapse()` and run at open, records lapses, and a
write on the layer records any due lapse first. Release it as 0.12.0 with the test changes
above. Treat retention the same way in a second item, after this one.

If approved, the implementation is about four items of 45 minutes: (1) `until` as the lapse
time and the computed lapse on every read, with the inverted tests; (2) the take and the
other writes recording due lapses first, with the race test; (3) `next_lapse()`, the sweep at
open, and the binary's door scheduling it; (4) `cacheable_until` on leased reads, the README,
ACCESSIBILITY.md and the release.
