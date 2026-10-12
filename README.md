# ikigai-layers

**Shared spaces between people, their agents and their machines, where who said what is
stamped by the host, what may be said is a closed shape, and who may see what is a
capability you can read off a file.**

Bringing a collaborator into your work usually means one of two bad trades: give them a
seat in your tools (and with it everything else in there), or carry messages by hand
between two worlds. A **layer** is the third option: one independent, named space
(`urn:layer:talk`) that the people, the LLM agents acting for them and their automation all
read and write through the same doors, under grants that say exactly which layers each one
holds and nothing more.

- **A layer is configuration, not code.** One TOML file picks an access model, names the
  closed SHACL shapes every write must conform to and the JSON-LD context that is the
  layer's egress allowlist. A new channel for a new collaboration is a file.
- **Attribution is not optional and not forgeable.** The door that authenticated the
  caller mints its principal; the host stamps it (`prov:wasAttributedTo`), the kernel
  clock's time (`dcterms:created`) and the entry's position in the feed (`ik:seq`) into
  every entry, and refuses any of the three if a writer sends them.
- **Append-only by construction.** There is no edit and no delete. A correction is a new
  entry that `ik:supersedes` the old one, only ever its own author's.
- **Taking work is claiming it.** In a task layer, a take is atomic: of two participants
  taking one offer, exactly one wins, the other is told who did, and a receipt naming the
  winner is written before either hears back. Reports on an offer are addressed to its
  offerer, and only its taker can send them.
- **Isolation is the default, and enforced.** A layer belongs to one collaboration unless
  its own definition records the decision to share it. A manifest that mounts someone
  else's layer stops the host from starting and the grant generator from writing.
- **Nobody polls.** Every layer has a gapless, cursor-resumable feed with a long poll, and
  one read can wait on all the layers a participant holds.
- **An agent's tool list is its grant.** Each layer action is its own endpoint declaring
  its layer's exact capability, so the MCP tools a participant's Claude sees are exactly the
  layers that participant holds.

## The resources

| resource | verb | what it is | capability |
| --- | --- | --- | --- |
| `urn:layer:{layer}` | Source | the definition, an `ik:Layer` | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:shapes` | Source | the closed admission shapes | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:context` | Source | the egress context (JSON-LD, no `@vocab`) | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:say` | Sink | append one entry; answers its IRI | `urn:cap:layer:say:{layer}` |
| `urn:layer:{layer}:entries` | Source | entries `after=` a sequence, `from=` a participant, `type=` a class | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:entry:{id}` | Source | one entry (never changes) | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:graph:{slug}` | Source | one author's entries | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:latest` | Source | the newest entry `about=` an IRI that nothing supersedes | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:tuples` | Sink | `out`: drop one tuple; answers its subject | `urn:cap:layer:out:{layer}` |
| `urn:layer:{layer}:tuples` | Source | `rd`: the pending tuples (or one, or those a `match=` ASK holds for); `state=taken`: the receipts | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:tuples` | Delete | `take`: remove one tuple by its subject, atomically | `urn:cap:layer:take:{layer}` |
| `urn:layer:{layer}:receipt:{id}` | Source | one take: who, when, and the tuple as taken (never changes) | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:feed` | Source | events `after=` a sequence, `wait=` up to 60 s | `urn:cap:layer:feed:{layer}` |
| `urn:layers:feed` | Source | one wait over several feeds, `after=talk:12,notes:3` | `urn:cap:layer:feed:{each}` |
| `urn:layers:participants` | Source | who each principal is, across the host (the operator's) | `urn:cap:layers:read:participants` |
| `urn:layers:me` | Source | who the door says the caller is: its principal and, when it is a participant, its kind and the collaborations granting it anything (`as=application/json` for a client) | `urn:cap:collab:read:*`, which every generated grant holds |
| `urn:collab:{name}` | Source | a collaboration's manifest | `urn:cap:collab:read:{name}` |
| `urn:collab:{name}:participants` | Source | who takes part in that collaboration: each participant its manifest grants anything, as the participants graph describes it | `urn:cap:collab:read:{name}` |

A channel layer binds `say`, `entries`, `entry`, `graph` and `latest`; a tuplespace layer
binds `tuples` and `receipt`; every layer binds its definition, shapes, context and feed.

Endpoint ids are `layer-{layer}-{action}` (`layer-talk-say`), so MCP tools are
`layer-talk-say__sink`, `layer-tasks-bobby-tuples__delete` and so on. Layer names are
lowercase `[a-z0-9-]`, at most 24 characters, which keeps the longest tool name inside
MCP's 64.

This version builds two access models: **channel** (append-only entries, one graph per
author) and **tuplespace** (offers taken atomically, reports addressed to their offerer,
with receipts). The **graph** model (each author replaces their own graph) is not built; a
definition naming it is refused, never served as something it is not.

## Configuration

One directory, the layer host's config home:

```text
<config dir>/
  layers/talk.toml           a layer's definition
  layers/msg.shapes.ttl      the closed shapes it names
  layers/msg.context.jsonld  its egress context
  participants.ttl           who each principal is
  collabs/bobby.toml         a collaboration's manifest
  enrollment.toml            certificate fingerprints (read by the generator only)
```

A layer:

```toml
# layers/talk.toml   ->  urn:layer:talk
model   = "channel"
shapes  = "msg.shapes.ttl"       # closed SHACL shapes: what may be written
context = "msg.context.jsonld"   # the egress allowlist
entry_max = "16KiB"              # optional; the default
shared  = ["bobby", "chris"]     # optional: the collaborations that may mount it
```

A task layer:

```toml
# layers/tasks-bobby.toml   ->  urn:layer:tasks-bobby
model     = "tuplespace"
shapes    = "tasks.shapes.ttl"
context   = "tasks.context.jsonld"
addressed = true                 # a tuple with ik:addressedTo is taken only by an addressee
```

`addressed` is a tuplespace's option and `about` a channel's; naming one on the other
model is refused, like a misspelled key.

Every node shape must be `sh:closed true` and the shapes must target at least one class;
an entry must be typed with a targeted class and only those (SHACL answers "conforms" for
what nothing targets, so the host refuses that first). The context may not use `@vocab`,
`@base`, `@import` or a remote context: it is an allowlist.

The participants:

```turtle
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix ik:   <https://ikigai-rs.dev/ns#> .
<urn:collab:participant:bobby>        a foaf:Person .
<urn:collab:participant:bobby-claude> a prov:SoftwareAgent ; prov:actedOnBehalfOf <urn:collab:participant:bobby> .
<urn:collab:participant:brian>        a foaf:Person .
```

A participant is `urn:collab:participant:{name}`, or a machine named the way its door
names it, `urn:iki:host:{host}:client:<fingerprint>`.

A collaboration:

```toml
# collabs/bobby.toml   ->  urn:collab:bobby
[[mount]]
layer = "talk"
bobby        = ["read", "feed", "say"]
bobby-claude = ["read", "feed", "say"]
brian        = ["read", "feed", "say"]
```

## Enrollment is generated

```text
ikigai-layers grants <config dir> [--out <dir>] [--check]
```

writes the door's `grants.json` (one grant per participant: the union of its verbs over
every manifest naming it, plus `urn:cap:collab:read:{name}` for each) and `clients.json`
(each certificate fingerprint from `enrollment.toml`, or from a machine's own name, mapped
to its grant with `principal` = the participant's IRI; no `default`, so a certificate
nobody enrolled is refused). ikigai-cli's door re-reads both on every connection, so an
edit takes effect without a restart. A configuration that breaks the isolation rule writes
nothing.

## Writing an entry

```text
$ cat note.ttl
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:layer:new> a ik:Message ; ik:body "The tuplespace arc is next." .

$ cat note.ttl | ikigai -c 'sink urn:layer:talk:say'
urn:layer:talk:entry:01m4hhr0z80000000000001
```

Turtle or JSON-LD, about `<urn:layer:new>` (or one blank node). What is stored:

```turtle
<urn:layer:talk:entry:01m4hhr0z80000000000001> a ik:Message ;
    ik:body "The tuplespace arc is next." ;
    prov:wasAttributedTo <urn:collab:participant:brian> ;
    dcterms:created "2026-10-10T00:00:01.000Z"^^xsd:dateTime ;
    ik:seq 1 .
```

And a follower that was waiting on `source urn:layer:talk:feed after=0 wait=50` wakes with

```text
1 2026-10-10T00:00:01.000Z say urn:layer:talk:entry:01m4hhr0z80000000000001 urn:collab:participant:brian
```

## Offering and taking work

A tuple is one subject that its dropper names, under the layer's own IRI and never used
there before:

```text
$ cat offer.ttl
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
<urn:layer:tasks-bobby:offer:01m4hhr0z8> a ik:Offer ;
    dcterms:title "Layer vocabulary" ; ik:brief "Add the layer terms." ;
    ik:repository <https://github.com/ikigai-rs/ikigai-core> .

$ cat offer.ttl | ikigai -c 'sink urn:layer:tasks-bobby:tuples'
urn:layer:tasks-bobby:offer:01m4hhr0z8
```

The host stamps who dropped it, when and its feed position into the tuple, as for an
entry. Sending the same tuple again (a retry after a lost reply) is one record, answered
the same way; anything else under a used name is a Conflict.

Taking it is the claim:

```text
$ ikigai -c 'delete urn:layer:tasks-bobby:tuples urn:layer:tasks-bobby:offer:01m4hhr0z8'
```

answers the tuple, and a second taker is refused with a Conflict that names the first.
Before the reply, the host commits a receipt: the take as a `prov:Activity` that
`prov:used` the offer, `prov:wasAssociatedWith` the taker, ended at a time, numbered in the
feed, with the tuple as taken. A taker that lost its reply reads
`source urn:layer:tasks-bobby:tuples state=taken tuple=<offer>`.

Reports (`ik:Progress`, `ik:Release`, `ik:Handback` with `ik:evidence`) name the offer
with `ik:offer` and are addressed to its offerer with `ik:addressedTo`. The host admits a
report only from the participant whose take of that offer it holds a receipt for, in an
addressed layer only when it is addressed to that offer's offerer alone, and there only an
addressee can take it. The feed records `out` and `taken`
events, with the tuple's subject and who did it.

## Hosting it

A host library: a host (ikigai-cli's `serve`) opens a `Host` over its configuration and
state directories and binds `ikigai_layers::space(&host)`, alone, on a kernel with a clock.
State is plain files: each entry is written once to
`<state>/{layer}/graphs/{author}/{id}.ttl`, and each feed is `<state>/{layer}/feed.log`.

- **One host per state directory.** The host holds `<state>/host.lock` exclusively while it
  lives; a second host over the same directory, in this process or another, is refused with
  the holder's pid. The operating system releases the lock when the holder exits, however it
  exits.
- **Admission is the host's own call.** The host links
  [ikigai-shacl](https://crates.io/crates/ikigai-shacl) and validates every write itself,
  so a door binds no `urn:shacl:validate`: that endpoint declares no capability, and bound
  in a door it would be offered to every participant. A layer whose shapes the validator
  cannot compile stops the host at open.
- **Who a participant can learn about.** `urn:layers:participants` is the whole host's
  roster and no generated grant reads it; each participant reads
  `urn:collab:{name}:participants` for its own collaborations, under the
  `urn:cap:collab:read:{name}` its grant already carries.
- **Who a participant is, by its own door's word.** `urn:layers:me` answers the principal the
  door minted for the caller, so a client never infers its identity from the attribution on
  records it already holds. It is readable under any generated grant, needs no new token, and
  lists only the collaborations whose exact tokens the caller holds. A request that names no
  principal (root names nobody) is refused, never answered with an empty graph.

A tuplespace layer fronts an [ikigai-intray](https://crates.io/crates/ikigai-intray) space
at `<state>/{layer}/tuples/`, which the host links and calls under its own
`urn:cap:space:*`: participants never hold a space capability, and the space is never
bound into the kernel. Intray's take is a rename, which is what makes it atomic. Receipts
are `<state>/{layer}/receipts/{id}.ttl`. Keep the state directory out of any intray root a
reactor watches.

## License

Licensed under either of MIT or Apache-2.0, at your option.
