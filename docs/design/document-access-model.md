# Document access model

This research spike proposes a third model beside today's channel and tuplespace.
A document layer holds one collaboratively edited CRDT document. It accepts
attributed, sequenced update entries and exposes a materialized snapshot that
editors can load and extend. Schedule implementation last, after runtime layers,
streaming, and the graph model.

The baseline is visible in [model configuration](../../src/config.rs) and its
[`only_built_models_load` test](../../tests/config.rs). The configuration accepts
channel and tuplespace, explicitly refuses graph as unbuilt, and rejects an
unknown CRDT model. The [existing feed](../../src/feed.rs) supplies an ordered
cursor and host-stamped event metadata. A document can reuse those conventions.
Its sequence records admission order. The CRDT determines how concurrent edits
merge.

## Library choice

Start the interoperability and sizing trial with Yrs on the Rust host and Yjs in
the browser. Use Automerge as the comparison. Yrs is the first candidate because
a document checkpoint can discard deleted content with garbage collection (GC)
enabled, and the attributed layer log can carry the audit history separately.
Automerge is a strong candidate if native access to historical CRDT states is
required. Pin released versions and an update encoding only after the trial.
[Document updates in Yjs](https://docs.yjs.dev/api/document-updates),
[Automerge binary document format](https://automerge.org/automerge-binary-format-spec/).

| Criterion | Automerge | Yjs and Yrs |
| --- | --- | --- |
| Rust and browser | The native Rust core and the JavaScript package use the same implementation via WebAssembly. Browser builds need to initialize the wasm module. [Automerge library initialization](https://automerge.org/docs/reference/library-initialization/) | Yrs is the native Rust implementation targeting Yjs binary interoperability. Browser clients can use JavaScript Yjs directly, or Yrs through the optional ywasm bindings. Test the supported editor operations across the pinned versions. [Y CRDT project](https://github.com/y-crdt/y-crdt) |
| Licensing | MIT for the Rust core and JS package. [Rust manifest](https://github.com/automerge/automerge/blob/main/rust/automerge/Cargo.toml), [JS license](https://github.com/automerge/automerge/blob/main/javascript/LICENSE) | MIT for Yjs, Yrs, and ywasm. [Yjs license](https://github.com/yjs/yjs/blob/main/LICENSE), [Yrs manifest](https://github.com/y-crdt/y-crdt/blob/main/yrs/Cargo.toml), [ywasm manifest](https://github.com/y-crdt/y-crdt/blob/main/ywasm/Cargo.toml) |
| Snapshot size and compaction | Saving consolidates changes in a compressed binary document that retains the full change history. Retained history can grow while visible text stays the same size. [Binary document format](https://automerge.org/automerge-binary-format-spec/) | Encode a full state update from a loaded document. Merging encoded updates alone deduplicates without collecting deleted content. Loading with GC enabled can reclaim deleted payloads, with clocks, deletion metadata and any remaining structural items still contributing to size. [Document updates in Yjs](https://docs.yjs.dev/api/document-updates), [Yjs CRDT algorithm](https://github.com/yjs/yjs#yjs-crdt-algorithm) |

These are format properties. Relative byte size, encode and restore time, and
browser bundle cost remain unmeasured. Replay the same edit trace in both libraries
using identical batching and equivalent document structures. Include append-only
typing, repeated deletion and replacement over fixed visible text, and concurrent
offline forks with multiple replica IDs. At fixed entry counts, record visible
UTF-8 bytes, full checkpoint bytes, total retained log bytes, replay time, peak
memory, and compressed browser bundle size. Test the Yrs checkpoint with and
without GC, and apply edits from a replica that predates the checkpoint. Record
versions, GC settings, transport compression, and environment. Choose Yrs if the
pinned versions converge across Rust and browser and the measured storage and
restore costs meet the host's budget.

## Update admission and grants

Use existing layer-scoped capabilities. The proposed `update` grant
(`urn:cap:layer:update:{layer}`) lets an authenticated participant submit an
update to that layer's entire document and current generation. Any editor can
change or delete content originally submitted by another editor. Editors with
multiple devices can use distinct CRDT replica IDs. Treat those IDs, embedded
timestamps, and the authors of operations inside a batch as client-controlled.
The entry attests to who submitted the batch. A host checks the current grant
at submission, including when an offline editor reconnects. Revocation ends
future admission and authorized reads. Accepted edits and previously downloaded
copies persist.

Grant `read` for snapshots and update entry bodies, and `feed` for event
metadata and cursors. An interactive editor normally needs all three grants.
Use separate document layers when writers need different content boundaries.
Field-level roles, per-author editing, and write-once sections would require a
different operation model or semantic authorization. A document reader can
inspect the entire encoded checkpoint and update history available to them,
including old values retained by the codec. Keep public enrollment and sharing
explicit in the layer's configuration.

State the exception in the model description, layer definition exposed by the
door, and participant documentation:

> For a document layer, every submitted update envelope must conform to the
> layer's closed SHACL shapes. CRDT update bytes are opaque to SHACL. SHACL
> validation does not cover the edits inside those bytes or the merged document.

Constrain the envelope to one update, the target and generation, and a supported
codec version. Bind the target to the endpoint. Keep the bytes unchanged for
replay. Apply the layer's egress context to RDF metadata and serve CRDT bytes
through an explicitly granted binary resource. The context cannot redact fields
inside those bytes. Use host-stamped author, time, sequence, and entry IRI. Refuse
client-supplied host metadata. Separately cap encoded size, decoded work, and
document growth. Decode and apply to staged state using the pinned library and
reject malformed or unsupported updates and batches with unresolved dependencies.
Give the editor a way to fetch the latest state and resend a complete update.
Publish the entry and feed event only after durable admission, with retry
deduplication scoped to the principal, document generation, and request key.
Reusing a key with different bytes is a conflict. Failed admission leaves no
visible entry or cursor advance.

## Snapshots and catch-up

The durable log is authoritative. Serialize admission for each document and
materialize it by loading a verified checkpoint and replaying accepted entries
after its cursor. Coalesce checkpoint work per document and trigger it on a
bounded amount of replay work, such as entry count and cumulative update bytes.
Publish an immutable binary checkpoint with its generation, codec, checksum,
and last applied layer sequence as one atomic version. A checkpoint at sequence
S represents every accepted update through S. Cache the latest durable
checkpoint and in-memory materialized state by layer and generation, with a
bounded memory budget. Check authorization before serving cached bytes. Scope
client caches to the authorized participant and keep shared HTTP caches out of
the read path. Rebuild on corruption from a verified earlier checkpoint and its
tail, or from the retained log. Never claim a cursor ahead of the bytes served.

A new reader gets the snapshot and cursor S, subscribes to or pages the feed
after S, then fetches and applies those update entries. Persist the cursor
only after applying its event, and use the same cursor when switching from
catch-up to streaming. Updates accepted during snapshot delivery appear in
the replay after S. A reconnecting reader can resume from a locally saved
cursor in the same generation. A stale cursor, generation mismatch, or lost
local state calls for a fresh checkpoint and another tail replay. Preserve
unsubmitted local edits separately and merge them into the fresh state only
within the same generation. A later optimization could use a Yjs state vector
or Automerge heads to calculate missing CRDT state. Keep the layer cursor and
attributed entries as the audit protocol.

Checkpoint compaction replaces cached checkpoint versions and bounds replay.
Retain attributed entries in the first implementation. Pruning their payloads
would require an explicit retention contract, a durable checkpoint covering
the pruned range, a minimum resumable cursor, and a fresh-snapshot response for
older cursors. Define its effect on audit replay and offline edits first.
Deleting text or running GC alone cannot erase its copies in logs, backups, or
clients.

## Proposed follow-up items

- **Measure Yrs and Automerge checkpoints and browser interoperability.** Run
  the shared edit traces with pinned Rust and browser releases. Check convergence
  across native and browser replicas, including offline edits across GC, and
  record byte, memory, and time measurements before selecting a codec.
- **Define the document layer contract.** Specify grants, the closed SHACL
  envelope, codec and generation rules, size limits, and the published exception.
  Exercise denied writes, spoofed attribution, missing dependencies, and retries.
- **Prototype durable update admission and checkpoint recovery.** Verify
  that interrupted writes and checkpoint publication recover with consistent
  state, attribution, and cursors. Measure replay cost under sustained edits.
- **Implement document bootstrap and streaming resume.** Exercise racing writes
  during snapshot fetch, reconnect from a saved cursor, and checkpoint reload.
  Specify retention and resynchronization before enabling any log pruning.
