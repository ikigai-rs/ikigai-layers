# Design spike: layers created and changed at runtime

**Status:** accepted 2026-11-24, with the recommendations (see "Decisions" below). Nothing here is
built yet. This document changes no host code, client, shape or configuration (ledger #2010).

**The question.** Today a layer is files in the host's config home, edited by hand: a
definition (`layers/<name>.toml`), the shapes and context it names, a collaboration manifest
that mounts it, the participants graph and `enrollment.toml`, followed by a run of
`ikigai-layers grants` and, for a new layer, a restart of the host (README.md:96-108,
README.md:257-271, README.md:380-382). Adding one layer means several hand-edited files, a
generator run and a restart. This document designs the other way: the layer definition, its
shapes and context, the collaborations, the participants and the enrollment become
**resources the host serves and writes**, under a capability for *administering* layers
that is separate from *taking part* in one. The grants are derived from them, never edited.
Command-line tools are thin clients over those resources. The host's files stay the storage
and the backup form, but nobody edits them to change a running host.

**How facts are cited.** `file:line` refers to this repository at `003690a`. A claim about
another repository marked **(unverified)** was not checked against its source. Participants
in examples are `alice` and `bob`; the person running the host is "the operator".

## Summary

- **Every file the host reads becomes one keyed resource**, named in the singular, the type
  first and every partition by a named key, under `urn:layers:`:
  `urn:layers:layer:name:{layer}`, `urn:layers:shape:name:{name}`,
  `urn:layers:context:name:{name}`, `urn:layers:scheme:name:{name}`,
  `urn:layers:collab:name:{collab}`, `urn:layers:participant:name:{name}` and
  `urn:layers:enrollment:fingerprint:{fingerprint}`. The set that lists each is its singular
  type (`urn:layers:layer` lists every layer, and so on). The mapping from file to resource is
  one to one, so migration is a reading of the existing directory, not a conversion.
- **The grants are a composition, not a resource anyone writes.** `urn:layers:grant` and
  `urn:layers:client` answer what `grants::generate` derives (`src/grants.rs:142`), and the
  host writes `grants.json` and `clients.json` after every accepted change, as onboarding
  already does (`src/onboard/apply.rs:405-445`).
- **A write is checked as the host's open checks it, before anything is written.** The
  change is staged in a copy of the configuration and loaded with `Config::load`
  (`src/config.rs:1914`), the shape compiler and the egress check (`src/host.rs:273-280`),
  exactly as `onboard setup` stages today (`src/onboard/setup.rs:347-400`). A configuration
  that breaks the isolation rule (`src/config.rs:1200`) is refused at the write, in the same
  sentence the generator prints, and nothing is written.
- **Administration is its own capability family**, `urn:cap:layers:admin:*`, keyed by what it
  administers (a layer, a collaboration, the roster). It never implies `read`, `say` or any
  other verb on a layer, and no manifest grants it. This separates the two duties; it does
  not make an administrator harmless (section 4).
- **The door picks up changes without a restart in two different ways.** Grants, clients and
  the trust list are already read live (README.md:267-268, README.md:318-319), so a change to
  participants, enrollment or a mount takes effect at the next connection with no new
  mechanism. Layers themselves are not: the host's layer map is fixed at open
  (`src/host.rs:204`) and the door's endpoint space is built once from it
  (`src/endpoints.rs:126-140`). Creating, changing or deleting a layer live needs the host to
  hold a swappable snapshot of its layers and the door to swap in a rebuilt space
  (section 6). Until the door can, the write's answer says a restart is needed, as
  onboarding's status does.
- **Recommendation:** build the resources, the staged check and the derived grants first,
  with collaborations, participants and enrollment live from day one; make layer creation
  live second, once the door can rebind; keep deletion to "unmount, then retire the name"
  and leave the full removal semantics to ledger #2006. About ten items, listed at the end.

## Decisions (Brian, 2026-11-24)

The recommendations are accepted:

- **Build order:** the resources, the staged check and the derived grants first, with
  collaborations, participants and enrollment live from day one; live layer creation second,
  once the door can rebind.
- **Several files, one change:** the journal (`<config>/.change/`), not a generation directory.
- **Serving a layer created live:** the host swaps its layer snapshot and the door rebinds its
  space (option 2 in section 6). Until the door can, a layer write is stored and its answer says
  a restart is needed (option 3 as the fallback).
- **Deleting a layer:** refused while mounted, otherwise the name is retired and the state kept
  sealed; full removal semantics stay with ledger #2006.
- **Names:** the new resources are singular and keyed as written here; existing public names stay.

The follow-up items of section 10 are filed as ledger #4307 to #4317, in order. A management
console, out of scope here, is a design spike of its own (ledger #4318), started once those
items are done.

## 1. What a host reads today, and when

| file | what it is | read | changed live today? |
| --- | --- | --- | --- |
| `layers/<layer>.toml` | a definition (README.md:110-160) | at open (`src/host.rs:257`) | no: a new or changed layer is read at the next open (README.md:380-382) |
| `layers/<file>.shapes.ttl` | closed SHACL shapes, named by a definition as a plain file name (`src/config.rs:769-777`) | at open | no |
| `layers/<file>.context.jsonld` | the egress context, likewise named | at open | no |
| `layers/<file>.scheme.ttl` | a feed's concept scheme (README.md:238-255) | at open | no |
| `layers.toml` | the host's own options: `[match]`, `[onboard]`, `[join]` | at open | no |
| `participants.ttl` | who each principal is | on use (`src/host.rs:569`) | yes, by the host |
| `collabs/<collab>.toml` | a manifest: mounts and per-participant verbs | on use, checked against the open layers (`src/host.rs:574-592`) | yes, but its grants only through a `grants` run |
| `enrollment.toml` | certificate fingerprints per participant | by the generator and onboarding | yes, through a `grants` run |
| `entitlements.toml` | what verified entitlements grant | written by the host only (`src/onboard/apply.rs:405-445`) | yes |
| `grants.json`, `clients.json` | the door's enrollment, derived | by the door, on every connection (README.md:267-268) | yes |

Two facts shape the design. First, **a precedent already exists for the host writing its
own configuration at runtime**: an applied entitlement appends to `participants.ttl`, writes
`entitlements.toml` and regenerates the enrollment pair, all through a temporary file renamed
over the old one (`src/onboard/apply.rs:884-893`). Runtime administration generalizes that
path. Second, **the split between "read live" and "read at open" is real**, and it is the
same split the door's pickup has to follow (section 6).

## 2. The resources

The rule is the one this host already follows: a set lists, an atom holds state and is named
singular and keyed, and a composition is derived and never written. Every name a key takes
follows the short-name rule the files already follow (lowercase `[a-z0-9-]`; a layer at most
24 characters, README.md:73-77).

### Sets

| resource | verb | what it is |
| --- | --- | --- |
| `urn:layers:layer` | Source | every layer definition's IRI, with its model, kind and the collaborations mounting it |
| `urn:layers:layer` | Sink | create a layer: the definition, named by its `name` (refused when it exists) |
| `urn:layers:shape` | Source | every shapes document, with the layers that name it |
| `urn:layers:context` | Source | every context document, with the layers that name it |
| `urn:layers:scheme` | Source | every concept scheme, with the feeds that name it |
| `urn:layers:collab` | Source | every collaboration (the operator's view; a participant keeps `urn:collab:{name}`) |
| `urn:layers:participant` | Source | every participant; the same answer as today's `urn:layers:participants` (README.md:60), which stays |
| `urn:layers:enrollment` | Source | every enrolled fingerprint and the participant it names |

### Atoms

| resource | verbs | what it holds | file it is stored as |
| --- | --- | --- | --- |
| `urn:layers:layer:name:{layer}` | Source, Sink, Delete | the definition: every key the TOML takes today | `layers/{layer}.toml` |
| `urn:layers:shape:name:{name}` | Source, Sink, Delete | a closed shapes document | `layers/{name}.shapes.ttl` |
| `urn:layers:context:name:{name}` | Source, Sink, Delete | an egress context | `layers/{name}.context.jsonld` |
| `urn:layers:scheme:name:{name}` | Source, Sink, Delete | a feed's concept scheme | `layers/{name}.scheme.ttl` |
| `urn:layers:collab:name:{collab}` | Source, Sink, Delete | a manifest: its label and its mounts | `collabs/{collab}.toml` |
| `urn:layers:collab:name:{collab}:mount:layer:{layer}` | Source, Sink, Delete | one mount: the verbs each participant holds on one layer | one `[[mount]]` table of that file |
| `urn:layers:participant:name:{name}` | Source, Sink, Delete | one participant: its class, label and `prov:actedOnBehalfOf` | its statements in `participants.ttl` |
| `urn:layers:enrollment:fingerprint:{fingerprint}` | Source, Sink, Delete | one certificate and the participant it names | one line of `enrollment.toml` |

Notes on the choices:

- **Shapes, contexts and schemes are their own sets, not parts of a layer.** Today a
  definition names a file in `layers/`, and two layers may name the same one (README.md:843-847
  has two layers each naming their own; nothing stops two naming one). Keeping them as named
  documents keeps that, keeps the file mapping one to one, and lets one change of a shared
  shape be one write. The write's answer then lists every layer the change reaches.
- **A mount is an atom inside a collaboration** so that `grant` changes one participant's
  verbs on one layer without rewriting the whole manifest, and two administrators granting on
  different layers do not race on one document. Finer atoms (one per participant per mount)
  were considered and rejected: verbs on one mount are checked together (a feed refuses `say`
  to a non-publisher, README.md:251-253), so the mount is the smallest unit a check covers.
- **`urn:layers:layer:name:{layer}` is not `urn:layer:{layer}`.** The second is the
  participant's view of the definition, under `urn:cap:layer:read:{layer}`, in Turtle and JSON
  (README.md:43). The administrator's resource carries the same definition plus what a
  participant has no business reading: which collaborations mount it, its file names, its
  revision. Two names keep two capabilities from ever meeting on one endpoint.
- **One participant name, `urn:layers:participant:name:{name}`.** It is the name the
  existing `Exists` resource already answers at (README.md:62, README.md:321-345); the atom
  adds Source, Sink and Delete to it, each under its own capability (`Exists` stays the
  controller's, the other verbs need `admin:roster`).
- **A mount is keyed inside its collaboration.** Since a name never holds `:`,
  `urn:layers:collab:name:{collab}` and `urn:layers:collab:name:{collab}:mount:layer:{layer}`
  never capture each other's IRIs, but the bind order must put the longer template first, as
  `urn:collab:{name}:participants` is today (`src/endpoints.rs:187-198`).
- **Existing public names stay as they are.** `urn:layer:{layer}:…`, `urn:collab:{name}`,
  `urn:layers:participants` and their capabilities keep their names; moving them to this form
  would be a separate migration, with the old names kept as aliases.
- **`layers.toml` stays a file.** The host's own options (`[match]`, `[onboard]`, `[join]`)
  hold keys and budgets that size the host; they change rarely, and a change already needs
  `onboard setup` or an open. They are out of scope for a first version.

### Compositions

| resource | verb | what it is |
| --- | --- | --- |
| `urn:layers:grant` | Source | what `grants.json` holds, derived; never written |
| `urn:layers:client` | Source | what `clients.json` holds, derived; never written |
| `urn:layers:revision` | Source | the configuration's current revision (section 3) |
| `urn:layers:change` | Sink | several atom writes applied together or not at all (section 3) |

`urn:layers:change` exists because the useful changes are rarely one atom. Creating a layer
that anyone can use takes a shapes document, a context, the definition and a mount, and the
definition is refused without its shapes. The change takes a list of `(resource, verb, body)`
and is checked and applied as one; a single-atom Sink is the same thing with a list of one.

### Representations

- A definition, a manifest and a mount accept and answer **the TOML the file holds**
  (`as=application/toml`) and a JSON face with the same keys. The wire form and the storage
  form are one text, so a backup restores as written and a definition reviewed in a pull
  request is the definition the host serves. A Turtle face for definitions already exists for
  participants and stays theirs.
- Shapes and schemes are Turtle; contexts are JSON-LD, as the files are today.
- A participant is Turtle (its statements) or JSON (`{"name", "kind", "label", "onBehalfOf"}`).
- An enrollment is JSON (`{"fingerprint", "participant"}`) and one plain line.

Every plain answer follows ACCESSIBILITY.md: lines that start with a word that says what they
are (`layer`, `mount`, `grant`, `changed`, `unchanged`, `restart`), and a JSON face for each.

## 3. A write: checked, ordered and stored

### The check is the open's check, run before the write

A write is refused unless the configuration *after* it would open. The host:

1. takes the configuration lock (one writer at a time; the state directory already has one
   lock, `src/host.rs:281`, and the configuration gets its own);
2. compares the request's `if=` revision with the current one, and refuses a stale one,
   naming both;
3. stages a copy of the configuration with the change applied, as `onboard setup` stages
   (`src/onboard/setup.rs:347-400`);
4. runs on the stage everything the open runs before touching state: `Config::load`
   (layers, participants, publishers, manifests and isolation, entitlements and
   `count_readers`; `src/config.rs:1914-1930`), the shape compiler and the egress check for
   every layer whose shapes or context changed (`src/host.rs:273-280`), and the onboarding
   and join checks when the change touches a lobby, a grants layer or a public feed;
5. derives the enrollment from the stage (`src/grants.rs:240`);
6. only then writes: each changed file through a temporary file renamed over it, then
   `grants.json` and `clients.json`, then the new revision.

A refusal at any step writes nothing and answers the step's own sentence: the isolation
refusal reads, as it does from `grants` today, ``layer `talk` is mounted by alice-team,
bob-team but declares no `shared` list: …`` (`src/config.rs:1216-1222`). There is one set of
checks and no second implementation of any of them.

### Several files, one change

Step 6 renames files one at a time, and a crash between two renames would leave a
configuration that may not open. Two ways out were considered:

- **A generation directory** (`<config>/gen/<n>/` and a `current` link swapped atomically).
  Every change is atomic, but the layout changes for everyone, and every reader of the
  directory (the door, deployments that apply from git, README.md:492-496) has to learn it.
- **A journal** (`<config>/.change/`): the staged files are written there first, with a
  manifest of renames; the renames follow; the journal is removed last. At open, a complete
  journal is replayed and an incomplete one discarded. The layout is unchanged, and a host that
  never takes a runtime write never has a journal.

The recommendation is the **journal**, because it keeps the file layout the storage and
backup form exactly as it is.

### Revisions and hand edits

`urn:layers:revision` is a digest over the files the configuration reads, in a fixed order.
Every Sink and Delete takes `if=<revision>`, and a CLI reads the revision just before it
writes, so two administrators never overwrite each other silently.

Hand edits stay possible, since the files are still the storage: on a stopped host they are
read at the next open, as today. On a running host, the host recomputes the revision from
disk at each write; when the files changed beneath it, the write is refused, naming the
files that differ and saying that the host must be reloaded or the edit undone. The host
never merges a hand edit with a runtime write.

### What a definition change may change

Not every key may change on a layer that already holds state:

| change | live? | why |
| --- | --- | --- |
| `label`, `kind` | yes | description only |
| `entry_max`, `retain`, `lease_max`, `match`, `addressed` | yes | apply to the next write or sweep |
| shapes or context (or which document is named) | yes | new writes are admitted under the new shapes; stored entries stay as written, since a layer is append-only (README.md:24-25), so readers must expect entries that conformed to an earlier version |
| `shared` | yes, if every mounting collaboration is still listed | the isolation check runs on the stage |
| `publishers`, `scheme` | yes | checked as at open |
| `public` off | yes | revokes every identity joined for it, which today happens at the next open (README.md:463-466); a live version does it at the write |
| `model` | **no** | a channel's entries and a tuplespace's tuples are different state; make a new layer |
| `feed`, `lobby`, `join` on or off | **no**, in a first version | they change which endpoints exist and the onboarding and join state the host builds at open (`src/host.rs:259-266`) |

## 4. Capabilities: administering is not taking part

The family is `urn:cap:layers:admin:*`. Each is checked exactly, as the layer verbs are. Each
follows the resource names' form, with its one parameter last, since core matches a wildcard
only at the end (`urn:cap:layers:admin:layer:name:*` covers every layer).

| capability | lets its holder |
| --- | --- |
| `urn:cap:layers:admin:read` | read every set and atom above and the compositions (not the entries of any layer) |
| `urn:cap:layers:admin:create` | create a layer, a shapes document, a context or a scheme, under a name nobody holds |
| `urn:cap:layers:admin:layer:name:{layer}` | change or delete that layer's definition |
| `urn:cap:layers:admin:document:name:{name}` | change or delete that shapes document, context or scheme |
| `urn:cap:layers:admin:collab:name:{collab}` | change that collaboration's manifest and mounts: the `grant` command |
| `urn:cap:layers:admin:roster` | change participants and enrollment, host-wide |

The rules:

- **Administering grants no part in a layer.** `admin:layer:name:talk` does not include
  `urn:cap:layer:read:talk` or any verb; an administrator who should read the layer is granted
  `read` in a manifest like anyone else.
- **No manifest grants an administrative capability.** The operator names administrators in
  `layers.toml`, the way `join.count_readers` names who may read the join count
  (README.md:522-545), and the generator adds the capabilities to exactly those grants. A
  creator of a layer is given `admin:layer:name:{layer}` for it in the same change, recorded in
  the definition (`administrators = [...]`) so that it is derived, not remembered.
- **This is a separation of duties, not of trust.** Whoever holds
  `admin:collab:name:{collab}` can write a mount granting itself `read`; whoever holds
  `admin:roster` can enroll a certificate for any participant. The design makes those acts
  explicit, checked and recorded, not impossible. What it does guarantee is that taking part never confers administration: a
  participant's grant, however wide, holds no `admin:` capability unless the operator named it.
- **Every accepted change is recorded.** The host appends one entry per change to a channel
  of its own (an administration log, mounted for the administrators), naming the resources,
  the verb, the author and the revisions before and after. A collaboration's participants can
  read the changes to their own collaboration through a feed bound under
  `urn:cap:collab:read:{collab}`, which every member's grant already holds. That makes a
  self-grant visible to the people it concerns.
- **The onboarding controller keeps its own path.** An approved entitlement already sets up a
  node's grants without a person editing files (ledger #2009). It is not rerouted through
  `admin:roster`; an entitlement's participant stays the entitlement's alone, and the roster
  write refuses to touch a participant an entitlement holds, in the same words the apply uses
  today (README.md:298-317).

## 5. Command-line tools over the resources

The tools are thin clients: each is one resource read and one write with the revision read
just before. They hold no rule of their own, so a refusal is the host's sentence, printed as
given, and `--check` makes the same request with `check=true`, which runs steps 1 to 5 and
writes nothing.

```text
ikigai-layers create  <layer> --model channel --shapes msg --context msg [--kind talk] [--collab alice-team]
ikigai-layers set     <layer> <key>=<value> ...
ikigai-layers shapes  <name> <file.ttl>
ikigai-layers context <name> <file.jsonld>
ikigai-layers mount   <collab> <layer>
ikigai-layers grant   <collab> <layer> <participant> read,feed,say
ikigai-layers revoke  <collab> <layer> <participant> [verbs]
ikigai-layers enroll  <participant> <fingerprint>
ikigai-layers retire  <layer>
ikigai-layers show    <resource>
```

`create` with `--collab` and the documents is one `urn:layers:change`, so a layer never exists
unmounted or without its shapes. Each prints one line per resource changed (`changed`,
`unchanged`), one `grant` line per grant that changed, and a `restart` line when the door
has to be restarted for the change to be served (section 6). The same requests work through
any client of a layer door, `ikigai -c` included, since they are ordinary resources.

**Where the tool connects.** The existing `grants`, `onboard setup` and `descriptor` commands
work on a directory (README.md:257-262). The new ones work on a door, as a participant
does, with a certificate whose grant holds the administrative capabilities. A directory form
(`--dir`, writing through the same staged check, for a stopped host) keeps working for
bootstrapping and recovery.

## 6. How the door picks up a change without a restart

Three kinds of change reach the door three ways.

**Grants and clients: already live.** The door re-reads `grants.json` and `clients.json` on
every connection (README.md:267-268), and the host now writes them at every accepted change,
as onboarding does. A grant, a revocation, an enrollment or a new participant takes effect at
the participant's next connection. No new mechanism is needed.

A connection already open keeps the capabilities it was given at its handshake
**(unverified: how ikigai-cli's door holds capabilities on a live connection)**. For a grant,
that only delays it. For a revocation it is a gap, and the fix belongs in the host: on each
request, check the caller's capability against the current derived grant, as the host
already refuses a deny-listed identity on a connection it holds (README.md:455-458). That is
proposed as a follow-up, not assumed.

**The trust list: live by default.** A certificate enrolled at runtime has to reach the
door's trust directory for a stranger's handshake to succeed; the door reads it at every
handshake unless `onboard.trust_reload = "start"` (README.md:318-319). The enrollment write
follows the same rule and the same answer onboarding gives when a restart is needed. Reloading
trust lists in general is ledger #1825.

**Layers: not live today, and the hard part.** Two things are fixed at open:

- the host's layer map, `layers: BTreeMap<String, Arc<LayerState>>` (`src/host.rs:204`),
  filled once in `open_with` (`src/host.rs:309-330`);
- the door's endpoint space, which `space(&host)` builds by iterating that map once
  (`src/endpoints.rs:126-140`). Every layer action is its own endpoint declaring its own
  layer's capability, which is what makes an agent's MCP tool list its grant (README.md:35-37).

Three ways to serve a layer created at runtime were weighed:

1. **One template endpoint per action, dispatching to the current layer.** The space never
   changes. Rejected: an endpoint declares one capability when it is described, so a template
   over every layer either declares a family (and an agent's tool list stops being its grant)
   or declares nothing.
2. **The host swaps its layer map; the door swaps its space.** The host holds its layers as
   an immutable snapshot it replaces whole (`Arc<BTreeMap<..>>` behind a lock or an
   atomic swap), opening the new layer's state (`open_layer`, as at open) before the swap and
   dropping a deleted one's after it. It bumps a change signal the door watches, and the door
   builds `space(&host)` again and replaces the space it serves, so a new connection sees the
   new endpoints, and an MCP door tells its clients the tool list changed. This needs a
   space-replacement hook in the door **(unverified: whether ikigai-cli's `serve` can replace
   a bound space; it is assumed it cannot today)**.
3. **Restart the door.** Always correct and needed as a fallback.

The recommendation is **(2)**, with (3) as the fallback until the door has the hook: the
write is accepted and stored, and its answer carries a `restart` line saying the layer will be
served after the door restarts, exactly as onboarding's status says `restart` today. Nothing
is half-served: until the swap, the new layer does not exist for the door, and the stored
definition is what the next open reads. A change to an existing layer's shapes, context or
options goes through the same swap (a new `LayerState` with the new definition over the same
state directory), so the in-flight requests on the old snapshot finish under the old shapes
and every request after the swap is admitted under the new ones.

## 7. Deleting a layer

Removal semantics are ledger #2006; this design keeps to what an administrator's tool needs:

- **A layer still mounted is not deleted.** The delete is refused, naming each collaboration
  that mounts it, so the grants are taken away first and visibly, by `revoke` and an unmount.
- **Deleting retires the name.** The definition goes, the endpoints are unbound at the next
  swap, and the state directory stays, sealed, under the state directory but outside any
  layer's name (as a directory no definition names is neither read nor sealed today,
  `src/host.rs:249-250`). The name stays reserved: a later layer with the same name would
  mint `urn:layer:{layer}:entry:{id}` IRIs that collide in meaning with the old layer's, and
  entries cite each other by IRI. Re-using a name, purging state and what readers are told
  are #2006's to decide.
- **A public feed's deletion revokes its joined identities at the write**, as taking `public`
  off does.

## 8. Migration from the files a host reads today

There is no conversion. Every resource above is one file, or one part of a file, of the layout
in README.md:96-108, so a host opening over today's directory serves every resource at once:

1. **Read.** A host with this feature opens today's directory unchanged and serves the sets and
   atoms from it. `urn:layers:revision` is computed at open.
2. **Bootstrap administrators.** The operator adds an `[admin]` table to `layers.toml` naming
   the participants that administer, and runs `ikigai-layers grants` once; from then on the
   host writes the enrollment pair itself.
3. **Stop editing.** Changes go through the tools. A deployment that applies the config home
   from a repository keeps working only if it stops overwriting the files the host now writes:
   the same rule the README gives for `joined/` (README.md:492-496) extends to every resource
   here. The supported routes are (a) a repository that is the *backup*, refreshed from the
   host's files, or (b) a repository whose apply step is `ikigai-layers` requests against the
   door (a declarative "make the host's resources equal these files" command, proposed below),
   never a file copy over a running host.
4. **Hand-edited files that do not round-trip.** A runtime write rewrites a whole file, so
   comments and formatting in a hand-written definition or manifest are lost the first time a
   tool changes it. A mount write rewrites its manifest's file. The tool says so the first time
   (`rewrote <file>: its comments are not kept`) and keeps a backup beside it, as `onboard
   setup` backs up `layers.toml` (README.md:366-368).
5. **Shared documents.** A shapes or context file named by several layers maps to one
   `urn:layers:shape:name:{name}` named by all of them, as today; nothing has to be split.

A host without the feature, or a participant build (the `no default features` build in CI),
is unchanged: the feature lives behind the `host` feature, and no default behavior changes
until the endpoints are bound.

## 9. What is out of scope

- A management UI.
- Changing `layers.toml`'s `[match]`, `[onboard]` and `[join]` tables at runtime.
- Changing a layer's model, or turning `feed`, `lobby` or `join` on or off on a layer that
  exists.
- The full removal semantics of a layer, which are ledger #2006's.
- Any description of a particular deployment: the README is the only description of how live
  layers are deployed.

## 10. Proposed follow-up items

1. **The configuration resources, read-only.** Serve the sets and atoms of section 2 from
   today's files, with `urn:layers:revision` and the `urn:cap:layers:admin:read` capability,
   and an `[admin]` table in `layers.toml` that the generator turns into grants; no writes yet.
   *Built (ledger #4307): `src/endpoints/admin.rs`, `tests/admin.rs`, README "Reading the
   configuration".*
2. **The staged write path and the journal.** A host-side `apply_change` that stages a change,
   runs the open's checks and the derivation, and commits through a journal replayed or
   discarded at open; tests that an isolation breach, an uncompilable shape and a stale
   revision each write nothing.
3. **Mounts and grants written live.** `urn:layers:collab:name:{collab}` and its mount atoms as
   Sink and Delete under `admin:collab:name:{collab}`, with `grants.json` and `clients.json`
   written at each change; the `mount`, `grant` and `revoke` commands.
4. **Participants and enrollment written live.** The roster atoms under `admin:roster`, refusing
   a participant an entitlement holds, in the apply's words; the `enroll` command; the trust
   directory updated under `trust_reload`.
5. **The administration log.** One entry per accepted change in a host-owned channel, and a
   per-collaboration feed of the changes to that collaboration, readable under
   `urn:cap:collab:read:{collab}`.
6. **Revocation on an open connection.** Check each request's capability against the current
   derived grant, so a revocation does not wait for the participant to reconnect; measure what
   the check costs per request.
7. **A swappable layer snapshot in the host.** Hold the layer map behind an atomic swap, open
   a new layer's state before the swap and retire a deleted one's after, with a change signal;
   no door change, tested through the library.
8. **Rebinding the door's space without a restart (ikigai-cli).** A hook that rebuilds and
   replaces a bound space on the host's change signal, and an MCP tool-list-changed
   notification; until it ships, every layer write answers `restart`.
9. **Layers, documents and schemes created and changed live.** The definition, shapes, context
   and scheme atoms as Sink under their capabilities, `urn:layers:change` for a layer created
   with its documents and mount in one request, the live and refused changes of section 3's
   table; the `create`, `set`, `shapes` and `context` commands.
10. **Retiring a layer.** Delete refused while mounted, the name reserved and the state kept
    sealed outside any layer name, public joins revoked at the write; coordinated with ledger
    #2006.
11. **Applying a directory as a desired state.** An `ikigai-layers apply <dir>` command that
    reads a config home and makes the host's resources equal it through `urn:layers:change`,
    with `--check` printing the difference, for deployments that keep layers in a repository.
