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
| `urn:layer:{layer}:tuples` | Source | `rd`: the pending tuples (or one, or, where the layer sets `match = true`, those a `match=` ASK holds for); `state=taken`: the receipts | `urn:cap:layer:read:{layer}` |
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
MCP's 64, and never a Windows device name (`nul`, `con`, `com1`, …), since a layer's name
is also a directory; participant short names follow the same rule.

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
  layers.toml                optional: the host's own options (the `match` budget, below)
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
match     = false                # optional; the default: `rd match=` is refused
```

`addressed` and `match` are a tuplespace's options and `about` a channel's; naming one on
the other model is refused, like a misspelled key.

**`match` is off by default.** A `match=` template is the caller's own SPARQL ASK,
evaluated against every pending tuple: the store's limits bound a query's size and
nesting, and a cross join of triple patterns is a few hundred bytes and shallow, yet costs
about 8x more for each pattern added: seconds on one tuple at eight patterns, minutes at
ten, and again for every pending tuple (ledger #1327). So on a layer that does not say
`match = true`, `rd match=` is refused (`InvalidArgument` on `match`, naming the option)
and the tuples door does not declare `match` at all; `rd` without a template, `out` and
`take` are unchanged. Turn it on only for a layer whose every reader you would hand a
thread to.

**A template runs within one time budget per host** (ikigai-intray 0.1.55's, from
ikigai-store; ledger #1362). Its caller is answered with a `Timeout` at the budget; a
template that cannot be stopped at once (oxigraph checks its cancellation only where it
touches the data) goes on running and is counted as OVERDUE, and while `max_overdue` are,
every new `rd match=` on ANY layer of the host is refused at once as `Unavailable`. The
host builds the budget once and every tuplespace layer shares it, so the cap is the
host's, not each layer's:

```toml
# layers.toml (optional; every key defaults)
[match]
budget_ms   = 2000   # default 5000
max_overdue = 1      # default: a quarter of the machine's cores, at least 1
```

A template is evaluated under the host's own authority, so a `urn:cap:store:budget:<ms>`
grant a participant holds does not raise it: `budget_ms` is what every caller gets. On a
small machine the default cap can be 1, and then one abandoned template refuses every later
one until it ends: size `max_overdue` (and `budget_ms`) deliberately before you turn
`match` on. `Host::match_budget()` says how many are overdue now.

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

Turtle or JSON-LD, about `<urn:layer:new>` (or one blank node); JSON-LD nested more than
64 levels deep is refused before it is parsed. What is stored:

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

## A gateway between kernels

One kernel asks another to resolve a resource and gets the answer back, through layers, with
neither resolving into the other and neither adopting the other's wire, cache or security
model (ledger #1370; the first step of a gateway between ikigai and NetKernel). It is
configuration: the example in [`gateway/`](gateway/) is two kernels, `plasma` and
`netkernel`, each asking the other.

- **An ask layer per asker** (`gateway-plasma`): a tuplespace, `addressed = true`. The layer
  is the read boundary (whoever holds `read` on a layer reads all of it), so an asker's
  requests and answers are seen by that asker and the resolvers serving it, and by no other
  asker. A resolver sees every request on the layers it serves: it is the server.
- **An event channel per resource owner** (`gateway-events-plasma`): only the owner holds
  `say`, so nobody can invalidate another kernel's resources; every kernel that may ask the
  owner reads it.
- **Closed shapes** ([`gateway.shapes.ttl`](gateway/layers/gateway.shapes.ttl),
  [`gateway-events.shapes.ttl`](gateway/layers/gateway-events.shapes.ttl)) and **egress
  contexts** with no `@vocab`. The terms are layer-local under `urn:layers:gateway:` (`gw:`)
  beside core's `ik:verb`, `ik:mediaType`, `ik:offer`, `ik:addressedTo` and `ik:seq`.

| tuple | name | carries |
| --- | --- | --- |
| `gw:Request` | `urn:layer:{layer}:request:{correlation}` | `gw:correlation`, `gw:resource` (an IRI), `ik:verb` (one of `Source`, `Sink`, `Exists`, `Delete`, `Meta`), any number of `gw:argument` (`name=value`, text), at most one `ik:addressedTo` (the one resolver that may take it) |
| `gw:Reply` | `urn:layer:{layer}:reply:{correlation}` | `ik:offer` (the request), `ik:addressedTo` (the asker), `gw:correlation`, `ik:mediaType` (with parameters), `gw:bytes` (`xsd:base64Binary`) |
| `gw:Failure` | `urn:layer:{layer}:reply:{correlation}` | `ik:offer`, `ik:addressedTo`, `gw:correlation`, `gw:errorKind` (one of `unresolved`, `missing-argument`, `invalid-argument`, `endpoint`, `denied`, `not-found`, `conflict`, `timeout`, `unavailable`, `depth-exceeded`: ikigai-core's `Error::kind()`), and `gw:message`, `gw:argumentName`, `gw:target`, `gw:depth` as the kind has them |
| `gw:Invalidation` | an entry, said about `<urn:layer:new>` | `gw:resource` (what changed, the sayer's own), `gw:reason` |

**The asker is the host's stamp** (`prov:wasAttributedTo`), never a field. **The host
enforces the protocol**: the atomic take gives a request to exactly one resolver; an answer
is admitted only from the participant whose take of the request the layer holds a receipt
for, addressed to the asker alone, and only the asker can take it.

**A request runs under the resolver's authority, never the asker's.** No capability crosses
a layer. The resolver issues the request into its own kernel under the capability its
operator gave it, answers only the verbs its operator allows (by default `Source`, `Exists`
and `Meta`, none of which changes anything; a mutating verb is answered `denied`) and, when
its operator sets them, only names under given prefixes. The layer grant says who may ask;
the resolver says what asking can do.

The reference participant for an ikigai kernel is
[`ikigai_layers::gateway`](src/gateway.rs): an `Asker` drops a request and waits for its
answer with a deadline (withdrawing a request no resolver took in time), and a `Resolver`
with `serve`, `serve_pending` and `follow` takes requests, resolves them and answers. A
representation larger than the layer's `entry_max` (base64 costs a third) is answered as an
`endpoint` failure saying so.

### The wire contract a participant carries

Every step is an ordinary request at the layer host, so this is also what an HTTP front for
non-ikigai participants (phase 2, for NetKernel) must carry: a verb, an IRI and named
arguments in; a media type and bytes out; and for an error, its KIND and message and, for
`missing-argument` and `invalid-argument`, the argument's name, because a participant
branches on them (a `conflict` on a take is a lost race, a `not-found` is an answer not there
yet). The front authenticates the participant and mints its principal, exactly as the ikigai
door does; the participant never names itself.

An asker, on its own layer:

| step | request | answer |
| --- | --- | --- |
| ask | `Sink urn:layer:{layer}:tuples content=<gw:Request>` | `text/plain`: the request's subject. `invalid-argument` (`content`) when the shapes refuse it; `conflict` when the correlation id was used before (the same request again is a replay, answered the same) |
| wait | `Source urn:layer:{layer}:feed after=<cursor> wait=<seconds, at most 60> as=application/json` | `{"layer", "last", "events": [{"seq", "time", "kind", "iri", "by"}]}`: wakes on any event; the next cursor is `last` |
| take the answer | `Delete urn:layer:{layer}:tuples tuple=<urn:layer:{layer}:reply:{correlation}>` | `text/turtle`: the `gw:Reply` or `gw:Failure`, stamped. `not-found` while there is none; `conflict` once taken (a take whose reply was lost: read it back with `Source …:tuples state=taken tuple=<reply>`, the receipt holding it) |
| withdraw at the deadline | `Delete urn:layer:{layer}:tuples tuple=<the request>` | the request, when nobody took it; `conflict` naming the resolver that did |

A resolver, on each layer it serves:

| step | request | answer |
| --- | --- | --- |
| find requests | `Source urn:layer:{layer}:tuples after=<seq>`, or follow the feed's `out` events whose IRI is under `…:request:` | `text/plain`, a line per pending tuple: `seq created subject dropper` |
| take one | `Delete urn:layer:{layer}:tuples tuple=<request>` | `text/turtle`: the request, stamped with its asker. `conflict` naming the winner when another resolver took it; `denied` when it is addressed to another |
| answer | `Sink urn:layer:{layer}:tuples content=<gw:Reply or gw:Failure>` | `text/plain`: the answer's subject |
| say what changed | `Sink urn:layer:gateway-events-{owner}:say content=<gw:Invalidation>` | `text/plain`: the entry's IRI |

What crosses, Turtle or JSON-LD under the layer's own context (`Source
urn:layer:{layer}:context`); answers come back as Turtle:

```json
{ "@context": { "…": "the layer's context, inline" },
  "@id": "urn:layer:gateway-plasma:request:01m4rjm4zt",
  "@type": "Request",
  "correlation": "01m4rjm4zt",
  "resource": "res:/demo/hello",
  "verb": "Source",
  "argument": ["as=text/plain"] }
```

```turtle
<urn:layer:gateway-plasma:reply:01m4rjm4zt> a gw:Reply ;
    ik:offer <urn:layer:gateway-plasma:request:01m4rjm4zt> ;
    ik:addressedTo <urn:collab:participant:plasma> ;
    gw:correlation "01m4rjm4zt" ;
    ik:mediaType "text/plain" ;
    gw:bytes "aGVsbG8="^^xsd:base64Binary ;
    prov:wasAttributedTo <urn:collab:participant:netkernel> ;
    dcterms:created "2026-10-12T18:00:01.000Z"^^xsd:dateTime ;
    ik:seq 3 .
```

Not built yet, and stated so nothing is inferred from silence: the HTTP front itself; any
publication of invalidations from golden-thread cuts (the shape is here, the mapping is the
research question); binary arguments (an argument is text; a `Sink` of bytes is not carried);
a per-asker capability on the resolver side (every asker gets the resolver's one); and any
cleanup of an answer that arrives after its asker gave up (it stays pending, addressed to the
asker).

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
