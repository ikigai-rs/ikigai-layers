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
| `urn:layer:{layer}:feed` | Source | events `after=` a sequence, `wait=` up to 60 s | `urn:cap:layer:feed:{layer}` |
| `urn:layers:feed` | Source | one wait over several feeds, `after=talk:12,notes:3` | `urn:cap:layer:feed:{each}` |
| `urn:layers:participants` | Source | who each principal is | `urn:cap:layers:read:participants` |
| `urn:collab:{name}` | Source | a collaboration's manifest | `urn:cap:collab:read:{name}` |

Endpoint ids are `layer-{layer}-{action}` (`layer-talk-say`), so MCP tools are
`layer-talk-say__sink`, `layer-talk-feed__source` and so on. Layer names are lowercase
`[a-z0-9-]`, at most 24 characters, which keeps the longest tool name inside MCP's 64.

This version builds the **channel** model (append-only entries, one graph per author). The
**tuplespace** model (offers taken atomically, reports addressed to their offerer, with
receipts) is next; a definition naming it is refused until then, never served as something
it is not.

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

## Hosting it

A host library: a host (ikigai-cli's `serve`) opens a `Host` over its configuration and
state directories and binds `ikigai_layers::space(&host)` beside `ikigai-shacl` (admission
composes `urn:shacl:validate`) on a kernel with a clock. State is plain files: each entry is
written once to `<state>/{layer}/graphs/{author}/{id}.ttl`, and each feed is
`<state>/{layer}/feed.log`; one host process per state directory. Admission fails closed:
without a validator bound, nothing is admitted.

## License

Licensed under either of MIT or Apache-2.0, at your option.
