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

Layers are inspired by Daniel Suarez's novel *Daemon*
([daniel-suarez.com](https://www.daniel-suarez.com/)).

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
| `urn:layer:{layer}:graph` | Source | a feed's entries in ikigai-feed's RDF form, with its scheme ([Feeds](#feeds)) | `urn:cap:layer:read:{layer}` |
| `urn:layer:{layer}:scheme` | Source | a feed's SKOS concept scheme ([Feeds](#feeds)) | `urn:cap:layer:read:{layer}` |
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
| `urn:layers:join:count` | Source | a host with a join only: three aggregate numbers about its joined readers ([Counting joined readers](#counting-joined-readers)) | `urn:cap:layers:read:join-count`, which no generated grant holds |
| `urn:layers:me` | Source | who the door says the caller is: its principal and, when it is a participant, its kind and the collaborations granting it anything (`as=application/json` for a client) | `urn:cap:collab:read:*`, which every generated grant holds |
| `urn:collab:{name}` | Source | a collaboration's manifest | `urn:cap:collab:read:{name}` |
| `urn:collab:{name}:participants` | Source | who takes part in that collaboration: each participant its manifest grants anything, as the participants graph describes it | `urn:cap:collab:read:{name}` |

A channel layer binds `say`, `entries`, `entry`, `graph` and `latest`; a tuplespace layer
binds `tuples` and `receipt`; every layer binds its definition, shapes, context and feed.
Every answer a participant reads also comes as JSON: see [JSON faces](#json-faces).

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
  enrollment.toml            certificate fingerprints (read by the generator and onboarding)
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

A channel may also be a **feed**, read by many and written only by its publishers
([Feeds](#feeds)):

```toml
# layers/releases.toml   ->  urn:layer:releases
model      = "channel"
shapes     = "msg.shapes.ttl"
context    = "msg.context.jsonld"
feed       = true                   # read-only for everyone but its publishers
publishers = ["brian"]              # required, at least one: manifest names or full IRIs
scheme     = "releases.scheme.ttl"  # required: one skos:ConceptScheme, with its concepts
public     = true                   # optional, default false: advertised for joining
shared     = ["audience"]           # the isolation rule is unchanged
```

`feed` is a channel's option only, and `publishers`, `scheme` and `public` are refused
without it. A publisher must be a name `participants.ttl` holds, and a manifest that grants
`say` on a feed to anyone else is refused, naming both, by the host's open and by
`ikigai-layers grants` alike. The scheme file sits beside the definition; every concept needs
a `skos:prefLabel` and a `skos:notation`, and every `skos:broader` must name a concept of the
same scheme.

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

## Onboarding a node by itself

Two layers, both configuration (the example pair is [`onboard/`](onboard/), with its
`[onboard]` table and a manifest), let a node with a brand-new certificate ask to join and be
let in on a signed decision, with no file copied by hand (ledger #2009):

- **A lobby** is a channel marked `lobby = true`. A door may admit a certificate nobody
  enrolled to `say` on it and nothing else: `Host::lobby_capability(host, fingerprint)` is the
  capability to mint for it, under the principal `urn:iki:host:{host}:client:{fingerprint}`.
  Every lobby is held to 16 KiB an entry, 5 says an hour from one certificate and 30 from
  everyone together (refused as `Unavailable`, naming the bound). Its shapes admit an
  `onb:EnrollmentRequest` about `<urn:iki:onboard:request:{fingerprint}>` (or
  `<urn:layer:new>`), and the host checks it beyond them: the posted certificate's leaf DER
  must hash to `onb:fingerprint`, which must be the poster's own and the one in the subject.
- **A grants layer** is a channel only `onboard.controller` says on (the host refuses a
  manifest that grants anyone else `say` there, and any other author). Its entries are
  `onb:Entitlement`s signed with [ikigai-sign](https://crates.io/crates/ikigai-sign) under the
  controller's key over `onboard::signing_bytes`. The host verifies each new one against
  `onboard.controller_key` and applies what verifies: the participant into `participants.ttl`,
  its grants into `entitlements.toml` (which the grants generator reads beside the manifests),
  `grants.json` and `clients.json` regenerated, and the certificate, taken from the lobby
  request the entitlement names, into `onboard.trust_dir` as `{fingerprint}.crt`. An applied
  entitlement also grants `read` on the grants layer, so the node reads its own. A newer one
  for the same fingerprint must `ik:supersedes` the current one (so a replay changes nothing),
  and one with no `onb:layerGrant` revokes. A participant is one certificate's: an entitlement
  naming a participant another certificate already holds, through `enrollment.toml` or an
  earlier entitlement (expired ones included, until revoked), is refused with
  ``onb:principal <participant> is already held by certificate <fingerprint>, enrolled by hand
  in enrollment.toml: invite this node under another name, or take that certificate out of
  enrollment.toml first`` or ``…, entitled by <entry>: invite this node under another name, or
  revoke that entitlement first``. An entitled node's grant comes from its entitlement alone,
  so an entitlement naming a participant any manifest grants, whether or not a certificate
  holds it yet, is refused with ``onb:principal <participant> is granted by manifest `<name>`,
  and an entitled node's grant comes from its entitlement alone: invite this node under another
  name, or take that participant out of that manifest first``. That is checked each time the
  host applies, so a manifest line added for an entitled participant takes its entitlement out
  of force. One naming `onboard.controller` is always refused: ``onb:principal <participant> is
  onboard.controller, which an entitlement never names: invite this node under another name``.
  Moving a certificate to a new participant supersedes its own
  entitlement; giving a participant a new certificate is a revocation, then a new entitlement.
- **`urn:layer:onboard:status`** (readable under `urn:cap:layer:say:{grants}`, the
  controller's) says what was seen, applied, superseded, expired and refused, and why, and
  whether the door must be restarted (`onboard.trust_reload = "start"`; cli's door reads its
  trust list at every handshake, so by default nothing is restarted).

With ikigai-cli at or after the release carrying
[f1d2124](https://github.com/ikigai-rs/ikigai-cli/commit/f1d2124b6eff40512aed346fd2a8ffa3ef917771),
a QUIC door with a lobby admits a stranger's unlisted, in-window client certificate with
`say` on the lobbies only. A door without a lobby still refuses it.

### Setting a host up for it, in one command

```text
ikigai-gonk onboard init --yes | ikigai-layers onboard setup <config dir> \
    --door-dir <the door's quic cert dir> --door-name <the door's --name> --controller-key -
```

`onboard setup` is the layer host's half of gonk's `onboard init` (ledger #2125), and nothing
in it is edited by hand:

- the lobby and grants layers and the `onboard` manifest are written from the copies in
  [`onboard/`](onboard/), embedded in the binary; the manifest names the controller. A file
  already there that differs is kept, and the output says where it differs;
- the `[onboard]` table is appended to `layers.toml`, with a backup beside it
  (`layers.toml.{unix seconds}.bak`), and the controller's public key is written beside it as
  `controller.pub.pem`. The key is a PEM file, or `-` for stdin, and the whole output of
  `ikigai-gonk onboard init` may be piped in: its PEM and its `controller = "…"` line are read
  from it (`--controller <participant>` names the controller otherwise). A table already there
  is never rewritten; it is compared, and every difference is reported;
- `trust_dir` is the door's own `clients/`, derived from `--door-dir`, which must be a door's
  QUIC certificate directory (it holds `server.crt` and `server.key`), or setup refuses. There
  is no default: a wrong trust directory leaves an approved node in the lobby with no error;
- `--door-name` is refused when the lobby would refuse every stranger at such a door (the rule
  of `onboard::lobby_principal`: lowercase `[a-z0-9-]`, at most 48 characters);
- `grants.json` and `clients.json` are regenerated by what `ikigai-layers grants` runs.

Every write is made first in a staged copy of the configuration and loaded with the host's own
checks, so a refusal writes nothing. The output says whether the door must restart: new layers
and a new `[onboard]` table are read when the host opens, while manifests, the enrollment pair
and (with `trust_reload = "live"`) the trust directory are read live. A rerun with everything in
place writes nothing and says so.

`--check` (with `--door-dir` and `--door-name`) is read-only, and exits non-zero on each way a
node would be left in the lobby: no lobby layer, no `[onboard]` table or one that does not load
(a missing or unreadable controller key among them), a `trust_dir` that is not the door's
`clients/`, a directory that is not a door's, a bad door name, a `grants.json`/`clients.json`
pair the manifests do not derive, and a manifest granting `say` on the grants layer to anyone
but the controller. Setup runs the same check after it writes.

## Public layers and joining

A feed that says `public = true` may be read by a certificate nobody enrolled, and a **join
lobby** lets one ask to, with no person deciding (ledger #3290):

```toml
# layers/join.toml   ->  urn:layer:join   (the example is onboard/layers/join.toml)
model   = "channel"
shapes  = "join.shapes.ttl"             # admit one class, onb:JoinRequest
context = "join.context.jsonld"
lobby   = true
join    = "public"

# layers.toml
[join]
host_key        = "/run/credentials/layers/join.pem"  # required: `openssl genpkey -algorithm ed25519`
join_idle_days  = 90     # a joined identity lapses after this long without a read
per_certificate = 5      # requests an hour from one certificate
per_address     = 20     # requests an hour from one source address, where the door says it
ipv6_prefix     = 64     # how many leading bits of an IPv6 address count as one address
per_hour        = 120    # requests an hour from everyone together
max_joined      = 10000  # live joined identities
```

Only a feed may be public (a plain channel saying `public` is refused), and a host has one join
lobby. A stranger's request, said with the capability `Host::lobby_capability` mints for it:

```turtle
@prefix onb: <https://ikigai-rs.dev/ns/onboard#> .
<urn:iki:onboard:join:{fingerprint}> a onb:JoinRequest ;   # or <urn:layer:new>
    onb:certificate "-----BEGIN CERTIFICATE-----…" ;
    onb:fingerprint "{lowercase hex SHA-256 of the leaf DER}" ;
    onb:layer <urn:layer:releases> .
```

The host checks that the poster holds the certificate (the fingerprint in its principal equals
the request's, and the posted PEM hashes to it) and that every `onb:layer` is public. **Anything
else refuses the whole request**, naming the layer, in the same words whether that layer is
private or absent, so a refusal never says which private layers exist. Otherwise it signs an
`onb:Entitlement` with `join.host_key` and answers the request synchronously, since the joiner
cannot read the lobby; `as=application/json` carries these members:

```json
{ "granted": ["urn:cap:layer:feed:releases", "urn:cap:layer:read:releases"],
  "entitlement": "urn:iki:host:edge:entitlement:01…", "principal": "urn:iki:host:edge:client:ab…",
  "expires": "2027-02-06T00:00:00Z", "layers": ["urn:layer:releases"],
  "@id": "urn:layer:join:entry:01…" }
```

The grant is live on the joiner's next connection, under the principal
`urn:iki:host:{door}:client:{fingerprint}` and never a participant's name. A door that mints
through `Host::lobby_capability` needs no change: for a joined certificate it now answers `read`
and `feed` on the joined layers and nothing else. `Host::door_capability` does the same and
also takes the connection's address, which the per-address limit counts.

**The guards.** The three rates are persisted, sealed at rest in `<state>/.join-limits.age`; they
count every request, and a request refused for a private layer counts too. A restart keeps the
per-certificate and global counts and begins the per-address count again (below).
`per_address` counts an IPv4 address, or an IPv6 address's first `ipv6_prefix` bits: a /64 by
default, the block one host commonly holds, so stepping through its addresses earns no new
count, and everyone behind one /64 shares one. Set 1 to 128, where 128 counts each full address.
A port is never counted, and an IPv4-mapped IPv6 address counts as its IPv4 address. A source
that is not an IP address is counted as the door gave it.
`max_joined` caps the live set. A fingerprint in `deny.txt` (in the config home, one
sha256 per line, `#` starts a comment) is refused at join, gets no capability at a connection,
and is refused on a connection it already holds; a list that does not read stops joins and
says which line. An identity lapses after `join_idle_days` without a read: its certificate
leaves the trust set, a read on a connection already open is refused, and it may join again.
`onb:expires` says when it lapses if it is never read; a read on a later day moves that later
on the host's record.

**What is kept.** No source address, in any file, log line or error. The host counts an
address by `HMAC-SHA256(key, address)`, where `address` is what the limit counts (the IPv4
address or the IPv6 prefix, with no port) and `key` is 32 random bytes held only in memory,
never written, and replaced at the first connection or request an hour or more after it was
drawn. For each request of the last hour, `.join-limits.age` holds its time, its certificate's
fingerprint and that hash; an entry stops counting after an hour and leaves the file at the
next counted request.

A hash matches only others made under its own key, so once the key is replaced nothing links
it to an address. The host forgets the replaced key's hashes then, from memory at once and
from the file at the next counted request or open. A connection made under the old key is not
counted by address until it reconnects, and one address can spend `per_address` once on each
side of a replacement. A restart starts a fresh key, which costs at most one hour of
per-address counting.

For each joined identity, its record holds when it joined and only the day of its last read:
`last_read` is the UTC midnight that began that day, written at most once a day, at that day's
first read. A record from an earlier version is rounded down and rewritten at open. The file's
own modification time, which the file system sets, still shows when that write happened.

Expiry counts from that midnight, so an identity read on a later day than its join lapses
`join_idle_days` after the start of that day, up to one day sooner than counting from the read
itself: with `join_idle_days = 1`, a last read at 23:59 UTC lapses at the midnight one minute
later. An identity not read after its join day lapses exactly when `onb:expires` says.

**Where it lives.** `<config>/joined/`, which only the host writes: `identities/{fp}.json`
(the record), `identities/{fp}.ttl` (the signed entitlement), `clients/{fp}.crt` (the trust set)
and a `grants.json` and `clients.json` in the form the door reads. It sits beside
`participants.ttl`, `grants.json` and `clients.json` and never inside them, so an apply from git
leaves it alone; when your apply deletes unknown files, exclude `joined/`.

A joined identity holds read and feed on public layers and nothing else, so a `say` anywhere
is refused and `urn:layers:me` (which needs a collaboration token it does not hold) is refused
too: it lists no collaboration for it.

### Counting joined readers

`urn:layers:join:count` answers the operator three numbers, and nothing else, as labeled lines
of `text/plain` or, with `as=application/json`, as JSON (ledger #3309):

```text
joined 12
read-in-7-days fewer than 5
read-in-30-days 8
```

`joined` is the live identities, the same set `max_joined` caps: a certificate on the deny list
is counted until its record lapses. `read-in-7-days` and `read-in-30-days` are those whose
record says a read in the last 7 or 30 UTC days, today among them. A record keeps one day for a
read and a join, so a join counts as a read on its day. **A count under 5, zero included, is
reported as `fewer than 5`**, so a count of one never singles anyone out. The answer carries no
fingerprint, no per-identity row and no address, and reading it writes nothing.

The numbers are **upper bounds on people**, since one person may hold several certificates.
**Collaborators' enrolled identities are not counted**, since their reads are not recorded.

It declares `urn:cap:layers:read:join-count`, which no generated grant holds and a joined
identity never does, so a reader and a collaborator are both refused it: the feeds carry no
reader counts. A host with no `[join]` table does not bind it.

### The door descriptor

```text
ikigai-layers descriptor <config dir> --door quic://host:port --door-dir <dir> --out <dir>
```

writes `layers.json` and `server.crt` into `--out`, for the deployment to publish at
`https://<host>/.well-known/ikigai/layers.json`. `url` is relative to the descriptor; `label`
is present for a layer whose definition has one:

```json
{ "door": "quic://host:port",
  "server_cert": { "url": "server.crt", "sha256": "<64 hex>" },
  "join": "urn:layer:join:say",
  "public_layers": [ { "name": "releases", "type": "ik:Feed", "read_only": true, "label": "Releases" } ] }
```

## Writing an entry

```text
$ cat note.ttl
@prefix ik: <https://ikigai-rs.dev/ns#> .
<urn:layer:new> a ik:Message ; ik:body "The tuplespace arc is next." .

$ cat note.ttl | ikigai -c 'sink urn:layer:talk:say'
urn:layer:talk:entry:01m4hhr0z80000000000001
```

Turtle or JSON-LD, about `<urn:layer:new>` (or one blank node); JSON-LD or Turtle nested more
than 64 levels deep is refused before it is parsed. What is stored:

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

## Feeds

A **feed layer** is a channel for announcements: `feed = true`, `publishers`, and a SKOS
`scheme` (see [Configuration](#configuration)). Its entries are written only by its
publishers and read by everyone holding its `read` grant.

- **Publishers only.** `say` from a principal not in `publishers` is refused (`Denied`, naming
  the layer as a feed), even under a grant holding `urn:cap:layer:say:{layer}`.
- **Membership is invisible.** A feed's definition is typed `ik:Feed` as well as `ik:Layer`
  and lists no `dcterms:isPartOf`; `urn:collab:{name}` and `urn:collab:{name}:participants`
  name only a feed's publishers, so no answer counts or names its readers.
- **One shared copy.** A door's kernel keys its cache by the caller's whole capability, so
  a feed's reads run in a kernel of the host's own, under the host's authority narrowed to
  `urn:cap:layer:read:{layer}` with no principal: one request has one cached answer for
  every reader, and the door checks each reader's own read token and answers from that copy
  without keeping one per reader. The reads hang from the say thread, and a publish cuts it
  and then computes the default `entries` page and the `graph`, so the first reader finds
  them cached. `Host::feed_computations(layer)` counts what was actually computed.
- **The scheme.** `urn:layer:{layer}:scheme` serves the `skos:ConceptScheme` as Turtle or
  JSON. An entry's `dcterms:subject` values must be concepts of that scheme, refused otherwise,
  naming the concept.
- **The graph.** `urn:layer:{layer}:graph` is the entries in
  [ikigai-feed](https://github.com/ikigai-rs/ikigai-linkeddata/tree/main/crates/ikigai-feed)'s
  RDF form (`rss:item`, `rss:title`, `content:encoded`, `dcterms:issued`, `dc:creator`,
  `dcterms:subject`, `dcterms:isPartOf <urn:layer:{layer}>`), unioned with the scheme, from one
  SPARQL CONSTRUCT kept as a file (`src/feeds/graph.rq`). An entry's title is its
  `dcterms:title` and its content its `ik:body`; `dcterms:issued` and `dc:creator` are the
  stamps the host wrote. Its JSON-LD face compacts without pruning scheme annotations.
- **Filtered entries.** `entries concept=<c>[,<c>...]` answers the entries whose
  `dcterms:subject/(skos:broader|^skos:narrower)*` reaches any given concept, from one
  SPARQL SELECT over the graph (`src/feeds/entries.rq`, property paths, no reasoner), keyed by the concepts and never
  by the caller. Both hierarchy directions are followed transitively; cycles terminate.
  Labels and `skos:exactMatch`/`skos:closeMatch` do not widen the filter. An unknown concept
  is `invalid-argument` naming it. Entries are ordered by their host-stamped sequence,
  oldest first. `after=<seq>` is exclusive; resume after the last entry received. All
  filters apply before `limit` (default 100, at most 1000), so a new publish between pages
  neither repeats nor skips an earlier entry. There is no `since` argument.
- **Atom.** `entries as=application/atom+xml`, with or without `concept=`, is an Atom 1.0
  document from the same SELECT through a stylesheet kept as a file (`src/feeds/atom.xsl`).
  If shapes allow several titles or bodies, Atom uses the SPARQL minimum of each, with
  one row per entry before paging. A missing body becomes empty text content.

A reader asks about a feed entry in its own Talk channel, not on the feed. Opening a
feed to a clean installation, and approving a join, are a door's concern: gonk's and the
Python client's, not this host's; `public = true` is only what the definition's JSON face
advertises.

## JSON faces

Every answer a participant reads also comes as JSON, so a participant with only an HTTP client
and a JSON parser can take part: no RDF parser is needed on its side. Each face is declared in
the resource's description (its `as` argument and outputs), so a door over QUIC, an HTTP
front, MCP and the action manifold all offer the same ones.

| answer | `as=` | body |
| --- | --- | --- |
| `entries`, `entry:{id}`, `graph:{slug}`, `latest` | `application/ld+json` | the entries, a graph (below) |
| `tuples` Source with `tuple=` or listing, `state=taken` too | `application/ld+json` | the tuples or receipts, a graph |
| `tuples` Delete (take) | `application/ld+json` | the tuple as taken, a graph |
| `receipt:{id}` | `application/ld+json` | the receipt, a graph |
| `tuples` Source listing | `application/json` | `{"layer", "tuples": [{"seq", "created", "subject", "dropper"}]}`, the text face's lines as objects; with `state=taken`, `{"layer", "receipts": [{"seq", "time", "subject", "taker", "receipt"}]}` |
| `say`, `tuples` Sink (out) | `application/json` | `{"@id": "<the IRI the write named>"}` |
| `urn:layer:{layer}` | `application/json` | `{"layer", "label", "model", "types": [...], "feed": bool, "public": bool}`, with `types` as compact terms (`"Layer"`, `"Feed"`); a feed's names no collaboration |
| `urn:layer:{layer}:scheme` (a feed) | `application/json` | `{"scheme": "<IRI>", "label", "concepts": [{"@id", "prefLabel", "notation", "broader": ["<IRI>"]}]}`, concepts by notation |
| `urn:layer:{layer}:graph` (a feed) | `application/ld+json` | the graph under the host's feed context (`src/feeds/graph.context.jsonld`), not the layer's egress context |
| `feed`, `urn:layers:feed`, `urn:layers:me`, `urn:collab:{name}` | `application/json` | as before |
| `urn:layers:join:count` | `application/json` | `{"joined", "readIn7Days", "readIn30Days"}`, each a number or, under 5, `"fewer than 5"` |

A face a resource does not offer is refused (`invalid-argument` on `as`), never answered in
another. One tuple (`tuple=`) is a graph, so it is refused as `application/json`, a list's
face. In the lists, `seq` is a JSON number, as in the feed.

A feed layer's `:graph` face preserves its scheme annotations, including properties outside
its context, under compact IRIs or full IRIs. The following egress rules apply to stored
entry and tuple faces.

**A graph** is the layer's egress context, PRUNED and compacted: the stored graph is
expanded, everything the context does not define is removed (a property, a type, a datatype;
a prefix is not a term), and the rest is compacted with that context. The host does this with
[ikigai-jsonld](https://crates.io/crates/ikigai-jsonld)'s prune, in a kernel of its own that
no door binds, so no participant can call the pruner. Its shape is fixed:

- one object with exactly two members: `@context` (the context it was compacted with) and
  `@graph`, an ARRAY of node objects in the answer's order (feed order for a list), so one
  node and many look the same, and none is an empty array;
- in each node, `@id` is the node's whole IRI, and `@type` a term of the context (a string
  for one type, an array for several); every other key is a term of the context, never a
  full IRI, and a property the context does not define is absent;
- a property with one value is that value, with several an array (a term defined with
  `"@container": "@set"` is always an array);
- an IRI value is a string when its term says `"@type": "@id"`, otherwise `{"@id": …}`;
- a typed literal whose term declares its datatype is its LEXICAL STRING: `"seq": "3"`,
  `"created": "2026-10-10T00:00:01.000Z"`, `"bytes": "aGVsbG8="` (base64 text). A datatype no
  term declares is removed with its value.

**The host's stamps always cross**, whatever the layer's context names: the host merges its
own terms into every layer's context for these faces.

| term | meaning | value |
| --- | --- | --- |
| `attributedTo` | `prov:wasAttributedTo`: who said or dropped it | the participant's IRI |
| `created` | `dcterms:created`: when, by the kernel clock | an `xsd:dateTime`, as a string |
| `seq` | `ik:seq`: its place in the layer's feed | an `xsd:integer`, as a string |
| `Activity` | `prov:Activity`: a receipt's type | |
| `used` | `prov:used`: the tuple a take removed | its IRI |
| `associatedWith` | `prov:wasAssociatedWith`: the taker | the participant's IRI |
| `endedAt` | `prov:endedAtTime`: when the take happened | an `xsd:dateTime`, as a string |

A receipt is a graph of two nodes: the `Activity` and the tuple as taken. A layer context may
define these terms only as the host does (the shipped contexts do, word for word); one that
defines one differently, maps one of these IRIs under another name (compaction would prefer
it), has a prefix that would shorten the IRIs the host names, or has a scoped `@context` (the
prune reads one set of terms) stops the host at open, naming the term.

```rust
use std::sync::Arc;
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, SystemClock, Verb};
use serde_json::Value;

# let dir = std::env::temp_dir().join(format!("ikigai-layers-readme-json-{}", std::process::id()));
# let (config, state) = (dir.join("config"), dir.join("state"));
# let _ = std::fs::remove_dir_all(&dir);
# std::fs::create_dir_all(config.join("layers")).unwrap();
# std::fs::create_dir_all(config.join("collabs")).unwrap();
# let write = |path: &str, text: &str| std::fs::write(config.join(path), text).unwrap();
# let prefixes = [
#     "@prefix sh: <http://www.w3.org/ns/shacl#> .",
#     "@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .",
#     "@prefix ik: <https://ikigai-rs.dev/ns#> .",
#     "@prefix prov: <http://www.w3.org/ns/prov#> .",
#     "@prefix dcterms: <http://purl.org/dc/terms/> .",
#     "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .",
# ].join("\n");
# let stamps = "[ sh:path prov:wasAttributedTo ; sh:nodeKind sh:IRI ] , \
#     [ sh:path dcterms:created ; sh:datatype xsd:dateTime ] , [ sh:path ik:seq ; sh:datatype xsd:integer ]";
# write("layers/msg.shapes.ttl", &format!("{prefixes}\n<urn:example:shape:message> a sh:NodeShape ; \
#     sh:targetClass ik:Message ; sh:closed true ; sh:ignoredProperties ( rdf:type ) ; sh:property \
#     [ sh:path ik:body ; sh:minCount 1 ; sh:datatype xsd:string ] , \
#     [ sh:path dcterms:subject ; sh:nodeKind sh:IRI ] , {stamps} ."));
# write("layers/tasks.shapes.ttl", &format!("{prefixes}\n<urn:example:shape:offer> a sh:NodeShape ; \
#     sh:targetClass ik:Offer ; sh:closed true ; sh:ignoredProperties ( rdf:type ) ; sh:property \
#     [ sh:path dcterms:title ; sh:minCount 1 ; sh:datatype xsd:string ] , {stamps} ."));
# write("layers/msg.context.jsonld",
#     r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#", "Message": "ik:Message", "body": "ik:body" } }"#);
# write("layers/tasks.context.jsonld", r#"{ "@context": { "ik": "https://ikigai-rs.dev/ns#",
#     "dcterms": "http://purl.org/dc/terms/", "Offer": "ik:Offer", "title": "dcterms:title" } }"#);
# write("layers/talk.toml", "model = \"channel\"\nshapes = \"msg.shapes.ttl\"\ncontext = \"msg.context.jsonld\"\n");
# write("layers/tasks.toml", "model = \"tuplespace\"\nshapes = \"tasks.shapes.ttl\"\ncontext = \"tasks.context.jsonld\"\n");
# write("participants.ttl", "<urn:collab:participant:brian> a <http://xmlns.com/foaf/0.1/Person> .\n");
# write("collabs/team.toml", "[[mount]]\nlayer = \"talk\"\nbrian = [\"read\", \"say\"]\n\n\
#     [[mount]]\nlayer = \"tasks\"\nbrian = [\"read\", \"out\", \"take\"]\n");
# write("state.key", age::secrecy::ExposeSecret::expose_secret(
#     &age::x25519::Identity::generate().to_string()));
# #[cfg(unix)] {
#     use std::os::unix::fs::PermissionsExt;
#     std::fs::set_permissions(config.join("state.key"), std::fs::Permissions::from_mode(0o600)).unwrap();
#     std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
# }
// The host's key: an age identity file (`age-keygen -o state.key`), whose path a deployment
// hands over. Every state file is sealed to it.
let key = ikigai_layers::StateKey::load(config.join("state.key")).expect("an age identity");
let host = ikigai_layers::Host::open(&config, &state, &key).expect("a configuration that checks");
let kernel = Kernel::new(Arc::new(ikigai_layers::space(&host))).with_clock(Arc::new(SystemClock));
// What a door mints for a participant holding these grants.
let brian = Capability::scoped([
    "urn:cap:layer:say:talk", "urn:cap:layer:read:talk",
    "urn:cap:layer:out:tasks", "urn:cap:layer:read:tasks", "urn:cap:layer:take:tasks",
].map(String::from))
.with_principal("urn:collab:participant:brian")
.unwrap();
let call = |verb: Verb, iri: &str, args: &[(&str, &str)]| -> Value {
    let mut request = Request::new(verb, Iri::parse(iri).unwrap());
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    let answer = block_on(kernel.issue(request, &brian)).unwrap();
    serde_json::from_slice(&answer.bytes).unwrap()
};

// say, answered as JSON: the IRI the host named.
let note = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
            <urn:layer:new> a ik:Message ; ik:body \"Next: JSON.\" ;\n\
            <http://purl.org/dc/terms/subject> <urn:example:topic> .";
let said = call(Verb::Sink, "urn:layer:talk:say", &[("content", note), ("as", "application/json")]);
let iri = said["@id"].as_str().unwrap().to_string();
assert!(iri.starts_with("urn:layer:talk:entry:"));

// The entries, as JSON-LD: always a top-level @graph array.
let doc = call(Verb::Source, "urn:layer:talk:entries", &[("as", "application/ld+json")]);
let entries = doc["@graph"].as_array().unwrap();
assert_eq!(entries.len(), 1);
let entry = &entries[0];
assert_eq!(entry["@id"], iri.as_str());
assert_eq!(entry["@type"], "Message");
assert_eq!(entry["body"], "Next: JSON.");
assert_eq!(entry["attributedTo"], "urn:collab:participant:brian");
assert_eq!(entry["seq"], "1"); // a lexical string, not a number
assert!(entry["created"].as_str().unwrap().ends_with('Z'));
// The context does not define dcterms:subject, so it did not cross.
assert!(entry.get("subject").is_none());
assert!(!doc.to_string().contains("urn:example:topic"));

// out, answered as JSON; rd's list as JSON; take as JSON-LD.
let offer = "@prefix ik: <https://ikigai-rs.dev/ns#> .\n\
             <urn:layer:tasks:offer:1> a ik:Offer ; <http://purl.org/dc/terms/title> \"JSON faces\" .";
let dropped = call(Verb::Sink, "urn:layer:tasks:tuples", &[("content", offer), ("as", "application/json")]);
assert_eq!(dropped["@id"], "urn:layer:tasks:offer:1");
let pending = call(Verb::Source, "urn:layer:tasks:tuples", &[("as", "application/json")]);
assert_eq!(pending["layer"], "tasks");
assert_eq!(pending["tuples"][0]["seq"], 1); // a list's seq is a number, as in the feed
assert_eq!(pending["tuples"][0]["subject"], "urn:layer:tasks:offer:1");
assert_eq!(pending["tuples"][0]["dropper"], "urn:collab:participant:brian");
assert!(pending["tuples"][0]["created"].is_string());
let taken = call(Verb::Delete, "urn:layer:tasks:tuples",
                 &[("tuple", "urn:layer:tasks:offer:1"), ("as", "application/ld+json")]);
assert_eq!(taken["@graph"][0]["title"], "JSON faces");

// A taker whose reply was lost reads the take back from its receipt: two nodes.
let receipt = call(Verb::Source, "urn:layer:tasks:tuples", &[
    ("state", "taken"), ("tuple", "urn:layer:tasks:offer:1"), ("as", "application/ld+json"),
]);
let nodes = receipt["@graph"].as_array().unwrap();
let activity = nodes.iter().find(|n| n["@type"] == "Activity").unwrap();
assert_eq!(activity["used"], "urn:layer:tasks:offer:1");
assert_eq!(activity["associatedWith"], "urn:collab:participant:brian");
assert_eq!(activity["seq"], "2");
assert!(activity["endedAt"].is_string());
assert!(nodes.iter().any(|n| n["@type"] == "Offer" && n["seq"] == "1"));
# drop(kernel);
# drop(host);
# std::fs::remove_dir_all(&dir).unwrap();
```

## A gateway between kernels

One kernel asks another to resolve a resource and gets the answer back, through layers, with
neither resolving into the other and neither adopting the other's wire, cache or security
model (ledger #1370). It is configuration: the example in [`gateway/`](gateway/) is two
kernels, `plasma` and `remote`, each asking the other.

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
| `gw:Request` | `urn:layer:{layer}:request:{correlation}` | `gw:correlation`, `gw:resource` (an IRI), `ik:verb` (one of `Source`, `Sink`, `Exists`, `Delete`, `Meta`), any number of `gw:argument` (`name=value`, text; its term is `@set`, so in JSON it is always an array, and absent when there are none), at most one `ik:addressedTo` (the one resolver that may take it) |
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

**A participant does not build the host** (ledger #1403). The host is the crate's default
feature, `host`; a tool that only talks to some layer door takes the crate without it and gets
the `gateway`, `name`, `rdf`, `id` and `time` modules, with no store, no space, no validator,
no JSON-LD processor, and so no oxigraph and no RocksDB:

```toml
[dependencies]
ikigai-layers = { version = "0.7.0", default-features = false }
```

A participant over the QUIC door went from 241 packages to 101 with it. The `Door` it hands
an `Asker` or a `Resolver` is one method, `call(Request) -> Result<Representation>`, over any
ikigai wire client.

### The wire contract a participant carries

Every step is an ordinary request at the layer host, so this is also what an HTTP front for
non-ikigai participants (not built yet) must carry: a verb, an IRI and named
arguments in; a media type and bytes out; and for an error, its KIND and message and, for
`missing-argument` and `invalid-argument`, the argument's name, because a participant
branches on them (a `conflict` on a take is a lost race, a `not-found` is an answer not there
yet). The front authenticates the participant and mints its principal, exactly as the ikigai
door does; the participant never names itself.

An asker, on its own layer:

| step | request | answer |
| --- | --- | --- |
| ask | `Sink urn:layer:{layer}:tuples content=<gw:Request>` | `text/plain`: the request's subject (`as=application/json`: `{"@id": …}`). `invalid-argument` (`content`) when the shapes refuse it; `conflict` when the correlation id was used before (the same request again is a replay, answered the same) |
| wait | `Source urn:layer:{layer}:feed after=<cursor> wait=<seconds, at most 60> as=application/json` | `{"layer", "last", "events": [{"seq", "time", "kind", "iri", "by"}]}`: wakes on any event; the next cursor is `last` |
| take the answer | `Delete urn:layer:{layer}:tuples tuple=<urn:layer:{layer}:reply:{correlation}>` | `text/turtle`: the `gw:Reply` or `gw:Failure`, stamped (`as=application/ld+json`: the same under the layer's context, see [JSON faces](#json-faces)). `not-found` while there is none; `conflict` once taken (a take whose reply was lost: read it back with `Source …:tuples state=taken tuple=<reply>`, the receipt holding it) |
| withdraw at the deadline | `Delete urn:layer:{layer}:tuples tuple=<the request>` | the request, when nobody took it; `conflict` naming the resolver that did |

A resolver, on each layer it serves:

| step | request | answer |
| --- | --- | --- |
| find requests | `Source urn:layer:{layer}:tuples after=<seq>`, or follow the feed's `out` events whose IRI is under `…:request:` | `text/plain`, a line per pending tuple: `seq created subject dropper` (`as=application/json`: `{"layer", "tuples": [{"seq", "created", "subject", "dropper"}]}`) |
| take one | `Delete urn:layer:{layer}:tuples tuple=<request>` | `text/turtle` (or `as=application/ld+json`): the request, stamped with its asker. `conflict` naming the winner when another resolver took it; `denied` when it is addressed to another |
| answer | `Sink urn:layer:{layer}:tuples content=<gw:Reply or gw:Failure>` | `text/plain`: the answer's subject |
| say what changed | `Sink urn:layer:gateway-events-{owner}:say content=<gw:Invalidation>` | `text/plain`: the entry's IRI |

What crosses, Turtle or JSON-LD under the layer's own context (`Source
urn:layer:{layer}:context`); answers come back as Turtle, or, asked `as=application/ld+json`,
as that context compacted (the reply below as `{"@context": …, "@graph": [{"@id": …,
"@type": "Reply", "offer": …, "mediaType": "text/plain", "bytes": "aGVsbG8=", "seq": "3",
…}]}`), which a participant with no RDF parser reads as plain JSON. Every term has ONE JSON
shape: `argument` is defined `"@container": "@set"`, so it is an array for one argument as
for several (and absent for none), and every other term is single-valued:

```json
{ "@context": { "…": "the layer's context, inline" },
  "@id": "urn:layer:gateway-plasma:request:01m4rjm4zt",
  "@type": "Request",
  "correlation": "01m4rjm4zt",
  "resource": "urn:example:hello",
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
    prov:wasAttributedTo <urn:collab:participant:remote> ;
    dcterms:created "2026-10-12T18:00:01.000Z"^^xsd:dateTime ;
    ik:seq 3 .
```

The shipped context is what fixes the JSON shape, so a participant can rely on it:

```rust
let context: serde_json::Value =
    serde_json::from_str(ikigai_layers::gateway::CONTEXT).unwrap();
assert_eq!(context["@context"]["argument"]["@container"], "@set");
```

Not built yet, and stated so nothing is inferred from silence: the HTTP front itself; any
publication of invalidations from golden-thread cuts (the shape is here, the mapping is the
research question); binary arguments (an argument is text; a `Sink` of bytes is not carried);
a per-asker capability on the resolver side (every asker gets the resolver's one); and any
cleanup of an answer that arrives after its asker gave up (it stays pending, addressed to the
asker).

## Hosting it

A host library: a host (ikigai-cli's `serve`) opens a `Host` over its configuration and
state directories, with the state's key, and binds `ikigai_layers::space(&host)`, alone, on a
kernel with a clock. State is files, sealed at rest (see below): each entry is written once to
`<state>/{layer}/graphs/{author}/{id}.ttl`, and each feed is `<state>/{layer}/feed.log`.

- **One host per state directory.** The host holds `<state>/host.lock` exclusively while it
  lives; a second host over the same directory, in this process or another, is refused with
  the holder's pid. The operating system releases the lock when the holder exits, however it
  exits.
- **Admission is the host's own call.** The host links
  [ikigai-shacl](https://crates.io/crates/ikigai-shacl) and validates every write itself,
  so a door binds no `urn:shacl:validate`: that endpoint declares no capability, and bound
  in a door it would be offered to every participant. A layer whose shapes the validator
  cannot compile stops the host at open. The host takes ikigai-shacl WITHOUT its `rudof`
  feature, so admission runs the subset validator (ikigai-shacl's README, "Two validators"),
  which refuses by name any construct outside its subset rather than skipping it: write a
  layer's shapes within the subset, or the layer never opens. A host binary that enables
  `ikigai-shacl/rudof` itself validates with rudof instead (cargo unifies the feature).
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
reactor watches. The space holds each tuple sealed: intray seals it as it stores it and opens
it wherever it reads it, through a codec the host gives it, so a `match=` template is
evaluated by intray itself, within its bounds and the host's one time budget. A pending tuple
that does not open refuses a `match=`, naming the tuple, rather than reading as absent. At
open, before it serves, the host discards any drop or take intray had staged when the host
last stopped: a staged take is one the host's receipts already record.

### State, sealed at rest

**Every file the host writes under `<state>/{layer}/` is age ciphertext** (age v1, X25519),
sealed to the host's own key and opened with it, so the files, a backup of them or an image
of the disk say nothing without the key (ledger #2011). An entry, a receipt (committed or
staged) and a tuple are each one age file; the feed stays a log of lines, each line one age
file in base64, so an append is still one synced write and a torn last line is still dropped
on open. age gives every file its own random file key, wrapped to the host's recipient:
envelope encryption. File names keep their suffixes; what is in a file says what it is.

- **The key is an age identity file** (`age-keygen -o state.key`), and the host is handed its
  PATH: `StateKey::load(path)`, then `Host::open(config, state, &key)`. Never an environment
  variable, so a deployment can hand the file over from an in-memory credential store
  (systemd's `LoadCredentialEncrypted`, say). A missing or unreadable file, or one holding no
  identity, is refused at open naming the file, never echoing a line of it.
- **Only its owner can reach the key (unix).** `StateKey::load` refuses, before reading it and
  naming the command that fixes it: a file owned by neither the host's account nor root; a file
  with any group or other permission (`chmod 600`, which `age-keygen -o` already sets; the one
  exception is a root-owned file in group root, readable by that group, as systemd's credential
  store writes it); and a directory holding the file that is owned by neither, or that group or
  others can write without the sticky bit (`chmod go-w`). A symbolic link is checked along with
  the file it reaches, and the directory of each. Directories above the one holding the file are
  not checked. Other platforms have no such check.
- **A wrong key opens nothing.** `<state>/key-check.age` is a fixed text sealed to the key,
  written after the first open with it succeeds. An identity that cannot open it is refused
  before anything else is read, so a wrong key never opens an empty layer.
- **A state directory written before 0.3.0 is refused** while it holds any plaintext file,
  naming one. Open it ONCE with `Host::open_with(config, state, &key, Migration::Reseal)`: every
  plaintext file is sealed in place, file by file, each written whole to a `.partial` file and
  renamed over the old one, and each layer is then opened again refusing plaintext, so the
  open fails if anything was left behind. A run that stops part-way is resumed by running it
  again. A tuple sealed is a new intray id, so it is stored again and the old copy taken; a
  stop between the two leaves it twice, and the next open keeps one.
- **Rotation is the same option.** The identity file may hold several identities: files are
  sealed to the FIRST and opened with any. Put the new identity first and keep the old one
  after it, open once with `Migration::Reseal` (everything sealed only to the old identity is
  sealed again, to the new one), then drop the old line.
- **Modes.** Files are created `0600` and directories `0700`, and every open makes what is
  already there the same (state written before 0.3.0, and the files intray creates with the
  process's umask).
- **What it does not cover.** The running host reads entries to validate them against the
  layer's shapes and opens tuples to evaluate `match=` templates, so it holds plaintext while
  it works; whoever holds the identity file reads everything; and a directory under the state
  directory that no layer definition names is not the host's state, so it is neither read
  nor sealed. Intray's `.err` notes beside a quarantined tuple stay plain text: an id and a
  fixed sentence. End-to-end sealing, which the host itself cannot read, is separate work.

```rust
use ikigai_layers::{Host, Migration, StateKey};

# let dir = std::env::temp_dir().join(format!("ikigai-layers-readme-key-{}", std::process::id()));
# let (config, state) = (dir.join("config"), dir.join("state"));
# std::fs::create_dir_all(&config).unwrap();
# std::fs::write(dir.join("state.key"), age::secrecy::ExposeSecret::expose_secret(
#     &age::x25519::Identity::generate().to_string())).unwrap();
# #[cfg(unix)] {
#     use std::os::unix::fs::PermissionsExt;
#     std::fs::set_permissions(dir.join("state.key"), std::fs::Permissions::from_mode(0o600)).unwrap();
#     std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
# }
let key = StateKey::load(dir.join("state.key")).expect("an age identity file");
// Once, over a directory a host before 0.3.0 wrote (or to rotate the key):
let host = Host::open_with(&config, &state, &key, Migration::Reseal).expect("sealed in place");
drop(host);
// Every open after that:
let host = Host::open(&config, &state, &key).expect("a sealed state directory");
assert!(state.join("key-check.age").exists());
# drop(host);
# std::fs::remove_dir_all(&dir).unwrap();
```

## Accessibility

People read this crate's write answers, plain listings, refusals and onboarding diagnostics, and
the `ikigai-layers` binary's terminal text. [ACCESSIBILITY.md](https://github.com/ikigai-rs/ikigai-layers/blob/main/ACCESSIBILITY.md)
says what that text is, the rules it follows, what is not tested, and how to report a barrier.

## License

Licensed under either of MIT or Apache-2.0, at your option.
