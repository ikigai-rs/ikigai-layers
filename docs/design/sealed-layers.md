# Design spike: sealed layers

**Status:** a design for a decision, not a decision. Nothing here is built. This document
changes no host code, client, shape or configuration (ledger #3780).

**The question.** Today a layer's access model gives an operator no grant on a layer between
two other participants, but the host seals its state at rest with its own key, on its own
machine, so the operator (or anyone with root there) *can* read every entry. The honest
statement is "the access controls give the operator no way in, and the operator does not
look". This document designs "the operator cannot": participants encrypt an entry's content
to the layer's other members before they say it, and the host admits, stamps, orders and
stores ciphertext it cannot open.

**How facts are cited.** `file:line` refers to this repository at `b35738e` unless another
repository is named. Other repositories are cited at these commits:

| repository | commit |
| --- | --- |
| [ikigai-encrypt](https://github.com/ikigai-rs/ikigai-encrypt) | `80270039634a26baed14cd1e1f6dfb93d8819a53` |
| [ikigai-sign](https://github.com/ikigai-rs/ikigai-sign) | `75a4e24ed72cbcea59ff436c153dfd4cb968cd47` |
| [ikigai-layers-client](https://github.com/ikigai-rs/ikigai-layers-client) | `d0ad6d9a97d4e4d4c92234c32fd425b49a3bdb98` |

A claim marked **(unverified)** was not checked against source. Sizes of the age format come
from the age v1 specification ([C2SP age](https://github.com/C2SP/C2SP/blob/main/age.md))
unless an ikigai-encrypt line is cited.

Participants are `alice`, `bob` and `carol`; the person running the host is "the operator".

## Summary

- A **sealed channel** is an ordinary channel whose closed shape admits one class,
  `seal:Sealed`, with one literal: the age ciphertext of the real entry. The host keeps
  stamping, ordering, bounding and storing; it cannot read, validate or search the content.
- Sealing hides **what was said**. It does not hide **who said something, in which layer,
  when, how big it was, in what order, who else is on the roster, or traffic patterns**.
- age has no sender authentication, and every member's recipient is public, so **anyone who
  knows the recipients, the operator included, can write a ciphertext the members will
  open**. A sender signature inside the ciphertext is therefore required, not optional.
- The central attack is the operator slipping in a key of its own. The operator controls
  `participants.ttl`, the manifests and the host, so **no key the host serves can be trusted
  on the host's word**. Members' keys are trusted only after a fingerprint comparison out of
  band, and the recipient set is each client's own pinned list, never the roster.
- Recommendation: a first version of **channels only, fixed membership, keys compared out of
  band, sender-signed and per-sender-counted entries, no history for late members**. It needs
  almost nothing from the host (a shipped shape, a layer option that refuses unsafe
  combinations, documentation) and most of its work in the two clients. About ten items of
  45 minutes, listed in section 9.

## 1. Threat model

### The adversaries

| adversary | holds | example |
| --- | --- | --- |
| **Honest but curious operator** | the host's configuration, `state.key`, the running process, logs | reads the state directory "just to see" |
| **Root on the host** | everything the operator holds, plus memory of the running process | a co-tenant, a cloud provider, an intruder who escalated |
| **Stolen backup** | the state directory, possibly `state.key` beside it | a disk image or an off-site copy |
| **Compromised host process** | the door's traffic, plaintext the host handles, the power to admit, drop, reorder and forge | a remote-code-execution bug in the door or a dependency |
| **Malicious operator** | all of the above, *and* the will to edit configuration and lie | wants to read a layer, or to make `alice` believe `bob` said something |

Today, all five read everything. The state at rest is age ciphertext sealed to the host's own
recipient (README.md:1032-1040, `src/at_rest.rs:378`), and the host is handed the identity's
path (README.md:1042-1046, `src/at_rest.rs:131`). So a stolen backup *without* `state.key`
learns nothing, but every other row holds or can reach the key; the README already says so
(README.md:1072-1077: "whoever holds the identity file reads everything ... End-to-end
sealing, which the host itself cannot read, is separate work").

With sealed layers, the content of a sealed entry is safe from all five, **provided** the
members' private keys never reach the host and members compared keys out of band (section 3).
A malicious operator who also controls a member's device, or runs an agent member on the host
(section 7), is outside what sealing can address.

### What sealing does NOT hide

Say this to every member, in these words or plainer:

- **Who said it.** The host stamps `prov:wasAttributedTo` from the door's authenticated
  principal into every entry (README.md:19-23, `src/channel.rs:632-660`). It must: the
  door authenticated the caller, and the grant check needs the principal.
- **Where and when.** The layer, `dcterms:created` (the host's clock) and `ik:seq` (its
  place in the feed) are stamped too (`src/channel.rs:646-657`). The feed line names the
  operation, the entry's IRI and its author (README.md:556-560).
- **How big.** The ciphertext's length is the plaintext's length plus a fixed overhead per
  recipient (section 2). Short replies, long messages and files are told apart by size.
  Padding (section 2) blurs this; it does not remove it.
- **To how many.** The number of recipient stanzas in an age header is visible to anyone
  holding the ciphertext. Their identities are not (an X25519 stanza carries an ephemeral
  share, not the recipient), but with a small roster the count often says who.
- **The roster.** `urn:layers:participants` is the operator's (README.md:59), and so are the
  manifests that grant each layer (`urn:collab:{name}`, README.md:63-64). The operator knows
  who *could* read a layer, and who has `read` and `say`.
- **Traffic patterns.** Every read, wait on the feed and write passes the door: who is online,
  who reads after whom, how fast `bob` answers `alice`.
- **The layer's existence, name and label.** They are configuration.
- **Whether anything is being said at all.** The host can always refuse, delay or drop
  (section 5).

Sealing also does not give **forward secrecy**. age encrypts each entry's file key to the
members' long-lived X25519 recipients; a member's private key stolen next year opens every
entry encrypted to it that the host still stores, and a channel stores them forever
(README.md:24: append-only, no delete).

## 2. The sealed entry's shape

### What a writer sends, and what the host stores

The writer seals the real entry (call it the *inner entry*) and says an *outer entry* that
holds only the ciphertext:

```turtle
@prefix seal: <https://ikigai-rs.dev/ns/seal#> .
<urn:layer:new> a seal:Sealed ;
    seal:age "YWdlLWVuY3J5cHRpb24ub3JnL3YxCi0+IFgyNTUxOSAu..."^^<http://www.w3.org/2001/XMLSchema#base64Binary> .
```

Stored, the host adds its three stamps as for any entry (README.md:549-553). The `seal:`
namespace is a proposal; it could equally be terms in `ik:` or `layers:` (`vocabulary.ttl`).

The proposed closed shape, within the subset validator's constructs already in use
(`gateway/layers/gateway.shapes.ttl:64-69` uses `sh:datatype xsd:base64Binary`,
`sh:maxLength`, `sh:nodeKind`, `sh:minCount` and `sh:maxCount`):

```turtle
<urn:ikigai:shape:sealed> a sh:NodeShape ;
    sh:targetClass seal:Sealed ; sh:closed true ; sh:ignoredProperties ( rdf:type ) ;
    sh:property
        [ sh:path seal:age ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:base64Binary ] ,
        [ sh:path prov:wasAttributedTo ; sh:minCount 1 ; sh:maxCount 1 ; sh:nodeKind sh:IRI ] ,
        [ sh:path dcterms:created ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:dateTime ] ,
        [ sh:path ik:seq ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:integer ] .
```

And its egress context declares the literal's datatype, so it crosses the JSON face as its
base64 string, the way `gw:bytes` does (README.md:713-716):

```json
{ "@context": { "seal": "https://ikigai-rs.dev/ns/seal#", "Sealed": "seal:Sealed",
  "age": { "@id": "seal:age", "@type": "http://www.w3.org/2001/XMLSchema#base64Binary" } } }
```

### Which visible fields remain, and why so few

| field | visible? | reason |
| --- | --- | --- |
| class `seal:Sealed` | yes | the host requires a targeted class (README.md:153-156); one class for every sealed entry, so the class says nothing about the content |
| content-type hint (message, file, correction) | **no** | the host has no use for it, and it is content: "a file" or "a correction" is exactly what the operator should not learn. It goes in the inner entry |
| recipient key ids | **no** | X25519 stanzas are anonymous by design; listing ids tells the operator who was left out. Recipients try each stanza (one X25519 operation per stanza, cheap at this group size). The recipient *set* is inside the signed plaintext instead (section 5) |
| `ik:supersedes` | **no** in the first version | the host checks a correction is the same author's (`src/channel.rs:13`), which it could still do, but the link says "this is a correction of that", content again. A sealed correction carries its target inside |
| `ik:about` (and `latest about=`) | **no** | the subject of an entry is content; `latest` (README.md:52) does not work on a sealed layer |
| the ciphertext | yes, opaque | |

**Binary age, base64 in an `xsd:base64Binary` literal, not ASCII armor.** ikigai-encrypt's
endpoint `urn:encrypt:encrypt` always answers armor (ikigai-encrypt `src/endpoints.rs:33-38,
290`), while its library `seal` writes binary age and `open` reads either (ikigai-encrypt
`src/age.rs:292-378`); a client uses the library face, as the host does for state at rest
(Cargo.toml:160-170). Armor is base64 with
a 64-column line break and a begin and end line, so it is strictly larger than the same bytes
as one base64 literal, and newlines inside a Turtle literal force long-string quoting. The
literal's datatype gives the host a lexical check for free (whether the subset validator
checks base64 lexical form, rather than only the datatype IRI, is **unverified**; the host can
check the decoded bytes start with `age-encryption.org/v1\n` itself in a few lines). Armor
stays the right form for a person pasting ciphertext by hand, which a layer client never does.

### What is inside the ciphertext

The plaintext is a small, fixed, byte-exact envelope, not RDF: Turtle has no canonical byte
form shared by the Rust and Python sides, and signing RDF means agreeing on a canonicalization
(the host's own `onboard::signing_bytes`, `src/onboard/mod.rs:220-250`, is sorted N-Triples
through oxttl's serializer, which Python would have to match byte for byte). An envelope of
bytes needs no such agreement:

```text
ikigai-sealed/1\n
layer <urn:layer:talk>\n
sender <urn:collab:participant:alice>\n
counter 17\n
seen 41 <base64 SHA-256 of the seq-41 entry's seal:age bytes>\n
to <alice's recipient> <bob's recipient>\n
type text/turtle\n
length 123\n
\n
<123 bytes: the inner entry, Turtle about <urn:layer:new>, conforming to the layer's inner shapes>
<padding: zero bytes to the next bucket>
signature <length>\n
<ikigai-sign's signature graph, Turtle, over every byte above the `signature` line>
```

ikigai-sign signs raw bytes and canonicalizes no RDF (ikigai-sign `src/lib.rs:359-362`,
`README.md:194-197`); its signature graph is one deterministic `sig:Signature` node with
`sig:algorithm`, `sig:signer`, `sig:value` and `sig:contentHash`, and no timestamp (ikigai-sign
`src/vocabulary.ttl:1-37`, `src/lib.rs:567-592`). Carrying the graph as ikigai-sign writes it
follows the onboarding precedent, where `onb:signature` is that graph as Turtle
(`src/onboard/mod.rs:123`). Python verifies it by reading `sig:value` and checking it with
`cryptography`'s Ed25519 against the sender's pinned key; `sig:signer` is informational and is
never the key a reader trusts (ikigai-sign `src/vocabulary.ttl:21-24`, `README.md:98-99`).

Field by field, section 5 says why each is there. The inner entry is an ordinary entry (an
`ik:Message` with `ik:body`, a file entry, a correction with `ik:supersedes`), and the
layer's inner shapes are published (as today, `urn:layer:{layer}:shapes`) so **members'
clients validate the inner entry after opening it**, with the same shapes the host would have
used. A sender's client validates before sealing; a reader's client validates after opening
and shows a non-conforming inner entry as refused, not as content.

**Padding.** Length is the biggest leak a sealed layer has. Pad the plaintext to the next of a
few buckets (for example 256 B, 1 KiB, 4 KiB, then the layer's limit). The cost is bytes, and
it blurs short versus long, not one message's exact length versus another's.

### What the host can still check, and what it no longer can

The host **still** checks: the caller's grant (`say`), that the entry is one subject of the one
class with one base64 literal of the right datatype, the size bound, its own stamps (refusing
them if sent, README.md:22-23), and, if the option in section 9 is built, that the decoded
bytes are an age v1 file.

The host **no longer** checks anything about the content: it cannot validate the plaintext
against any shape, cannot check that a correction is its author's own, cannot check a file's
digest (it never did, README.md:173-175), cannot evaluate `about`, `latest`, `type=` beyond
`seal:Sealed`, `match=`, or a feed's scheme. All of that moves to members' clients, and a
member whose client skips it sees whatever another member chose to seal. That is a real
change: today the shape is a guarantee the host gives every reader; on a sealed layer it is a
promise each sender's client makes, checked by each reader's client.

### `entry_max` and overhead

`entry_max` (default 16 KiB, `src/config.rs:51`) bounds the submitted Turtle or JSON-LD bytes
before parsing (README.md:158-163, `src/channel.rs:474`). For a sealed entry with `n`
recipients and envelope plaintext `P` bytes, by the age v1 specification:

| part | bytes |
| --- | --- |
| version line `age-encryption.org/v1\n` | 22 |
| each X25519 stanza: `-> X25519 ` + 43 base64 + `\n` + 43 base64 + `\n` | 98 per recipient |
| header MAC line `--- ` + 43 base64 + `\n` | 48 |
| payload nonce | 16 |
| payload: `P` bytes plus a 16-byte Poly1305 tag per 64 KiB chunk | `P + 16` while `P <= 64 KiB` |
| then base64 for the literal | x 4/3 |
| the outer Turtle around it (prefix, subject, class, predicate, datatype) | about 200 |

So for one chunk: `outer ≈ 200 + 4/3 x (102 + 98n + P)`.

| recipients `n` | envelope plaintext that fits in 16 KiB |
| --- | --- |
| 2 | about 11,840 bytes |
| 8 | about 11,250 bytes |
| 16 | about 10,470 bytes |
| 64 | about 5,760 bytes |
| 123 or more | none |

The envelope's own lines (layer, sender, counter, seen, one recipient line of about 63 bytes per
member, the signature graph of about 400 bytes) take a further few hundred bytes, so a two-member
layer carries roughly 11 KiB of inner Turtle, against 16 KiB unsealed. Armor would cost a
further line break per 64 characters and about 70 bytes of begin and end lines. A layer that
needs more raises `entry_max`; the client should read it from the layer's description
(`layers:entryMax`, README.md:158-160) and refuse to seal what will not fit, saying how much
too large it is, rather than send and be refused.

## 3. Keys

### Two keys per member

Each member holds:

- an **age X25519 identity** (`AGE-SECRET-KEY-1...`), whose recipient (`age1...`) others
  encrypt to, and
- an **Ed25519 signing key**, used through ikigai-sign, that signs sealed entries (section 5)
  and, later, key statements.

They are separate on purpose: rotating the encryption key (a lost laptop) should not change
the identity others have verified, and X25519 keys should not be reused for signing. A
first version could pin both keys out of band and skip in-band statements entirely.

### How others learn a member's key, and why not from the host

| option | who can lie about it |
| --- | --- |
| a recipient in `participants.ttl` | the operator: it is the operator's file (README.md:59, 177-186) and the onboarding controller writes into it (README.md:261) |
| a per-collaboration list in the manifest | the operator: manifests are the operator's configuration |
| self-published in the layer, signed with ikigai-sign | the operator can post a statement stamped as anyone, but cannot sign it with a key members already verified, so the question moves to "how do I trust the signing key" |
| compared out of band (in person, by phone, over a channel the operator does not run) | nobody, if the comparison is done |

**The central attack** is the operator adding a key of its own: a recipient line for itself in
`participants.ttl`, a new participant `mallory` granted `read` in the manifest (the operator can
always add a grant), or a substituted recipient for `bob` so that `alice` encrypts to the
operator instead. Every in-band channel for keys passes through the operator, so **in-band
distribution alone cannot stop this**. What stops it:

1. **The recipient set is the client's, not the host's.** Each member's client keeps a pinned
   list for the layer: participant IRI, age recipient, Ed25519 public key, and when it was
   verified. A client seals only to that list. A new participant in the manifest, a changed
   roster or a new `read` grant changes nothing a client does.
2. **Every entry to the pinned list is verified out of band once.** The client shows a short
   fingerprint of the member's keys (for example the first 20 bytes of SHA-256 over both public
   keys, as groups of digits, like a safety number) and each pair of members compares them by a
   path the operator does not control. Only then is the key pinned.
3. **The recipient set is signed into every entry** (`to` in the envelope), so a reader whose
   pinned list says `{alice, bob}` and who opens an entry addressed to `{alice, bob, mallory}`
   says so, plainly, instead of showing it as ordinary.
4. **A client never falls back to plaintext.** If the layer's shapes stop admitting
   `seal:Sealed` (an operator could swap the shapes file), the client refuses to send and says
   the layer is no longer sealed on this host, instead of sending in the clear. Whether a
   layer is sealed is the client's pinned fact, not the host's claim.

Later, with dynamic membership, a member's **signed key statement** in the layer (a
`seal:KeyStatement` naming the participant, its age recipient and its Ed25519 key, signed by
that Ed25519 key, and superseding the member's previous statement) lets a member rotate the
encryption key without another comparison: the new recipient is trusted because the
already-verified signing key signed it. A new signing key still needs a comparison.

ikigai-layers already has the pattern of a host-independent signing key: an entitlement is
signed with ikigai-sign under the controller's key, and the host verifies it against
`onboard.controller_key` (README.md:257-263, `src/onboard/mod.rs:63-67`). The difference is
that there the *host* is the verifier and trusts its own configuration; here the *members* are
verifiers and must not.

### Where each member keeps the private keys

Never on the layer host, and never in a file the host's door serves. A key on a machine the
operator administers is the operator's key.

- **The Rust cli** (`ikigai`): **(unverified)** this spike did not read the cli's
  repository. ikigai-encrypt takes a key only as an IRI resolved through the kernel
  (`urn:file:` or `urn:secret:*`), never by value, never echoes it, and mints no keys: that is
  left to the secret module (ikigai-encrypt `src/endpoints.rs:120-137`, `README.md:46-60`).
  Proposal: one identity per participant, not per layer, held in the cli's secret store as a
  `urn:secret:` resource, or as a file `0600` checked with the same owner and mode refusals the
  host applies to `state.key` (`src/at_rest.rs:210-297`, README.md:1047-1054).
- **The Python `ikigai-layers` client:** it keeps its certificate and key as `client.crt` and
  `client.key` (unencrypted PKCS#8), files `0600` in a directory `0700`, under
  `~/.ikigai/layers-client/<name>` or `--cert-dir` (ikigai-layers-client `certs.py:65-113`,
  `door.py:25, 43-47`). The two sealing keys go beside them, `seal.age` and `seal.ed25519`,
  under the same rules. The client certificate is an ECDSA P-256 key (`certs.py:1-62`), which
  ikigai-sign could verify as ES256 (ikigai-sign `README.md:18-23`), but it is a TLS key the
  door and the trust directory know, and reusing it for content signatures mixes two purposes;
  keep a separate Ed25519 key.
- **gonk's Layers page:** **(unverified)** this spike did not read gonk. A web page's keys
  live wherever gonk's server runs; if that is the layer host, the page cannot hold a key
  without defeating the point. First version: gonk's page does not decrypt; it shows the
  "sealed entry" line of section 7.

Losing the identity loses everything encrypted to it: the host cannot recover it and must not
be able to. Each client should say so when it creates a key, and offer an export the member
stores themselves.

## 4. Membership changes

**Fixed membership first.** Every change below is something a first version can refuse.

### Adding a member

The host cannot re-encrypt anything: it holds no key. A late member `carol` can read only
entries sealed to her after members pinned her key. To give her history, an existing member's
client would decrypt history and seal it again to `carol` (one large "history" entry, or one
per old entry, each signed by the re-sharer, not the original author, since the original
signatures cover the old recipient set). That is expensive, it says to the operator exactly
when history was shared and how much, and it asks a member to vouch for others' words. **Not
in the first version**: a late member sees history as sealed entries they cannot open, with
the line of section 7.

### Removing a member

They keep everything they already decrypted, and every ciphertext they could open stays
openable by their key forever. Each remaining client removes them from its pinned list, and
**new entries simply stop being encrypted to them**. age has no shared group key: every entry
gets its own random file key wrapped to each recipient, so there is nothing else to rotate.
"Rekey" here means exactly that: changing the recipient list. The operator should also revoke
their `read` grant, but sealing does not depend on it.

The hard part is agreement: until *every* remaining member's client drops `carol`, someone
still encrypts to her. The signed `to` line (section 5) lets each reader see who an entry
went to, so a lagging client is visible to the others.

A member whose own key is compromised rotates their age identity; others pin the new recipient
(out of band in a first version, by a signed key statement later). Entries already encrypted to
the old key stay readable to whoever holds it.

### Group size

Each recipient costs 98 bytes of header, 131 after base64, on every entry (section 2), and one
X25519 operation for the sender per recipient, and up to one per stanza for each reader trying
them. ikigai-encrypt's code sets no maximum number of recipients (it refuses a list holding a
low-order recipient, `src/age.rs:206-223`); whether the `age` crate it wraps has one is
**(unverified)**, and the age specification sets none. Proposal: a first version caps a sealed layer's pinned list at 16 members. Sealing is
for small groups; a group of hundreds is better served by a group protocol with a shared,
ratcheting key (MLS, RFC 9420), which this document does not propose.

## 5. Authenticity

### The stamps are the host's word

`prov:wasAttributedTo`, `dcterms:created` and `ik:seq` are the host's claims
(`src/channel.rs:632-660`). An honest host derives them from the door's authenticated
principal and its clock. A malicious operator can write any entry into the state directory
with any stamp, reorder the feed, drop entries, or show different members different feeds.

### A sender signature is required

age gives confidentiality and integrity, not authenticity: anyone who knows the recipients can
make a valid age file for them, and every recipient is public. So without a signature the
operator can write a ciphertext to `{alice, bob}`, stamp it as `alice`, and `bob` opens it.
**Every sealed entry carries an ikigai-sign Ed25519 signature inside the ciphertext**, over
the envelope's bytes, by the sender's pinned signing key. A reader that cannot verify it shows
the entry as refused ("did not verify"), never as `alice`'s.

The envelope's signed fields close the attacks a bare signature leaves open:

- `layer`: a member cannot take `alice`'s signed plaintext and seal it into another layer as
  hers (surreptitious forwarding).
- `sender`: must equal the host's `prov:wasAttributedTo`; a mismatch is shown as refused.
  This binds the host's stamp to the member's own claim.
- `to`: who the sender meant to read it, so a reader sees an unexpected recipient, or that a
  member was left out.
- `counter`: the sender's own count of sealed entries on this layer, starting at 1. A reader
  that sees `alice`'s 16 then 18 knows the host withheld 17; one that sees 17 twice knows
  something replayed it.
- `seen`: the highest `ik:seq` the sender had read when it sealed, with a digest of that
  entry's ciphertext. It ties each sender's view of the conversation to others' entries, so a
  reordering or a fork (the host showing `alice` and `bob` different histories) eventually
  shows up as a `seen` digest the reader does not have.

Signing inside the ciphertext rather than beside it keeps the signature from telling the
operator anything new, and keeps the signed bytes exactly the bytes the reader decrypted.

What this does **not** do: it cannot stop the host from withholding everything, or from
splitting members into two views indefinitely if they never compare; full detection needs
members to compare a feed digest out of band now and then. It also makes every sealed entry
**non-repudiable**: `bob` can prove to anyone that `alice` signed it. That matters for abuse
reports (section 8) and is a real loss of deniability; a deniable scheme is out of scope.

## 6. The three layer models

### Channels: the main case

Everything above. A sealed channel binds `say`, `entries`, `entry`, `graph` (one author's
entries) and the feed as today (README.md:66-67). `latest about=` and `entries type=` are
meaningless on it: everything is `seal:Sealed` and nothing is about anything the host can see.

### Tuplespaces

- **`rd match=` must stay off.** It is the caller's SPARQL ASK over a tuple's triples
  (README.md:122-130); over a sealed tuple it can only see the ciphertext. A sealed tuplespace
  must refuse `match = true` in its definition.
- **`out` and `take` work.** A tuple's subject is named by its dropper (README.md:569-570), and
  `take` removes one tuple by subject atomically (README.md:54-55); the host needs neither to
  see the content. Receipts hold the tuple as taken (README.md:595-599), which is the
  ciphertext: fine.
- **`addressed` works, with the addressee visible.** `ik:addressedTo` must stay a visible
  field, since the host takes an addressed tuple only for an addressee (README.md:114,
  `src/tuplespace.rs:21`). The same goes for `ik:offer` on reports, which the host checks
  against its receipts (README.md:616-621). So on a sealed tuplespace the host knows who offered
  work to whom, who took it, and which report answers which offer; only the brief, the
  evidence and the result are hidden. That is still useful (the work itself is private), but it
  is less sealed than a channel, and the shape has three visible fields instead of one.
- **Retention** (`retain`, README.md:600-614) works unchanged: it deletes by time.

Recommendation: not in the first version. It works, but every member type needs its own sealed
shape with the right visible fields, and the gateway layers built on tuplespaces
(README.md:864-886) have resolvers that must read requests, which makes them a poor first case.

### Feeds

A public feed is the opposite case. Its readers are whoever joins (README.md:361-365, 465): an
audience with no fixed membership cannot be a recipient list, and an entry readable by anyone
who joins is not secret from an operator who can join. A feed's `dcterms:subject` must also be
visible for the host to check it against the scheme and to serve Atom (README.md:625-664).
**Sealing a feed makes no sense**; a private announcement to a fixed group is a sealed channel
with one writer.

### Files

A file entry carries its content and a digest the host does not check (README.md:173-175):
readers must recompute it. The Python client's `ik:File` carries `ik:fileName`,
`ik:mediaType`, `ik:contentHash` (`sha256:<hex>`), `ik:byteSize`, `ik:encoding` (`text` or
`base64`) and `ik:content`, and checks size and digest itself before saving
(ikigai-layers-client `files.py:3-12, 323-385`). Sealed, the whole file entry (name, media type, content, digest)
goes inside the ciphertext, and nothing about it is visible, not even that it is a file (except
by size). The reader's client checks the digest after opening, exactly as today; the sender's
signature now also covers the digest, and age's authenticated encryption makes a tampered
ciphertext fail to open at all. The file still has to fit in `entry_max` after the overhead of
section 2 (about 8 KiB of file at the default limit, after base64 inside Turtle inside base64);
large sealed files need a separate design (an encrypted blob stored elsewhere, with its key and
digest in the sealed entry), which this document does not propose.

## 7. Clients

### One age implementation each side, pinned to the same bytes

- **Rust** uses ikigai-encrypt, the ecosystem's one age implementation (ikigai-encrypt
  `src/age.rs:4`), itself a thin face on the `age` crate 0.11 (ikigai-encrypt `Cargo.toml:37`).
  The host already seals with it (Cargo.toml:160-170).
- **Python**: do not write the age format by hand on top of `cryptography`'s X25519 and
  ChaCha20-Poly1305; the header MAC, the HKDF labels, the stanza grammar and the chunking are
  where hand-written age goes wrong. Use **pyrage**, the Python binding to rage, whose library is
  the same `age` crate ikigai-encrypt wraps, so both clients run one implementation of the
  format. The client today depends only on `ikigai[quic]` and `cryptography>=42` and has no
  age library (ikigai-layers-client `pyproject.toml:27-30`). That pyrage wraps the `age` crate
  at a compatible version, its wheel coverage, and its fit with the client's dependency policy
  are **(unverified)**. The alternative
  shared home is Python bindings to ikigai-encrypt itself (PyO3), which gives one
  implementation in both clients but adds a native build to the Python client; it is the right
  choice if pyrage is refused.
- **Test vectors** pin the two sides:
  1. age's own conformance vectors, the C2SP CCTV `age` set (header and payload edge cases), run
     against both implementations; ikigai-encrypt runs none today (its tests are `bodies`,
     `conformance`, `inputs`, `library` and `low_order`), so this is new;
  2. a fixture set owned by one repository (proposal: the Python client, which consumes the
     Rust host's vocabulary already) and copied byte for byte to the other with a checksum:
     fixed X25519 identities and Ed25519 keys; envelopes as exact bytes; their signatures
     (Ed25519 is deterministic, so a signature is a fixed vector); sealed entries made by
     Rust that Python must open and verify, and the reverse (age encryption is randomized, so
     the vectors run in the opening direction, plus a round trip each way in CI);
  3. refusal vectors: a wrong signature, a `sender` that differs from the stamp, a counter gap,
     a recipient outside the pinned list, an entry over `entry_max`, a plaintext that does not
     conform to the inner shapes. Each must produce the same verdict in both clients.

### What a person sees

The Python client's `read` prints a header line `{seq} {created} {attributedTo} {Kind} {entry
IRI}` and the details indented four spaces, and `follow` prints `layer seq time kind IRI who`
then a short summary (ikigai-layers-client `show.py:67-110`, `follow.py:9-18`). A sealed entry
keeps that header, with `Sealed` as its kind, so a listing stays one line per entry in the same
field order, and the indented line says, in words, what happened (ACCESSIBILITY.md, "What it
emits that people read"). For example:

```text
41 2026-11-16T10:00:00.000Z urn:collab:participant:alice Sealed urn:layer:talk:entry:01m...
    sealed entry: this device holds no key that opens it
```

| case | the indented line says |
| --- | --- |
| opened and verified | the entry as usual, with its real kind (`Message`, `File`) in the header |
| no key on this device | `sealed entry: this device holds no key that opens it` |
| not addressed to this member | `sealed entry: not sealed to you, for example because you joined after it` |
| signature fails, or sender differs from the stamp | `sealed entry that did not verify: do not trust it as alice's` |
| counter gap | `alice's sealed entry 18 arrived after her entry 16: entry 17 has not been shown to you` |
| unexpected recipient | `alice sealed this entry to bob and to mallory, who is not in your list for this layer` |
| inner entry does not conform | `sealed entry opened, but its content does not fit this layer's shapes: not shown` |

No case relies on an icon, a color or a lock symbol alone, and none hides the host's stamps:
who, when and where are known to everyone anyway, and saying so teaches the threat model. The
JSON face carries the same verdict as a field (for example `"sealed": "no-key"`), so an agent
reading JSON gets the same distinction.

### Agents

An agent participant holding a key sees plaintext, and so does whatever runs it: the model's
provider, the machine it runs on, its logs. An agent on the layer host itself makes the
operator a reader. A sealed layer's members should know which members are agents
(`prov:SoftwareAgent` and `prov:actedOnBehalfOf` in the participants graph, README.md:184), and
pinning an agent's key should say, in the comparison step, "this is an agent: whoever runs it
reads this layer".

## 8. Operations and policy

**The operator still controls** size (`entry_max`), rate, retention where a model has it, the
roster and every grant, whether the host runs at all, and removing a layer. It can deny
service: refuse, delay, drop or withhold entries (detected per section 5, not prevented).

**The operator can no longer** read content, moderate it, search it, run `match=` over it, or
answer an abuse report by reading. An abuse report on a sealed layer is a member disclosing
the plaintext; because entries are signed (section 5), a member can prove who wrote it, and the
operator's options are the ones it has today without reading: revoke a grant, remove a
participant, remove the layer. A policy for sealed layers should say this before anyone is
invited.

**Backups** keep working unchanged: a sealed entry is still sealed at rest to `state.key`, so a
backup is two layers of encryption, and a backup with `state.key` beside it reveals only what
section 1 lists as visible. Members' private keys are not in the host's backups and must not
be; each member backs up their own, or accepts losing what was sealed to them.

**What to tell a person invited to a sealed layer:**

> This layer is sealed: what you write here is locked on your own device so that only the
> people in it can open it, and the person running the host cannot read it, even with full
> access to the machine. They can still see that you wrote something, when, how long it was
> and who is in the layer, and they can stop the layer working. Before you write, compare the
> number your app shows for each person with that person directly, in person or over a call,
> not through this host. Keep your key safe: if you lose it, nobody can open what was sealed
> to you, and nobody can recover it for you. Anyone in the layer can show what you wrote to
> others, and prove you wrote it.

## 9. Recommendation

### A minimal first version

- **Channels only.** Tuplespaces, feeds, gateway layers and files over `entry_max` are refused.
- **Fixed membership.** The pinned list is set when the layer is created; changing it is a
  new layer.
- **Keys verified out of band**, both keys per member, by fingerprint comparison; no in-band
  key statements.
- **Sender-signed envelopes** with `layer`, `sender`, `counter`, `seen` and `to`, padded to
  buckets, inner entries validated by clients against the layer's inner shapes.
- **No history** for anyone added later (there is no later, with fixed membership).
- **gonk's Layers page** shows the "sealed entry" line and does not decrypt.

### What it costs

The host changes little. A sealed channel can be configured *today* with the shape and
context of section 2; what the host should add is a guard and a statement:

- a shipped `seal:` vocabulary, shape and context;
- a layer option `sealed = true` that refuses, at open, a definition whose shapes admit
  anything but `seal:Sealed` (or that is a tuplespace, a feed, public, or sets `about`), checks
  the decoded literal starts with the age v1 version line, and exposes `layers:sealed` in the
  layer's description; a client still trusts its own pin, not this flag (section 3);
- README text: the "what it does not hide" list, and the paragraph for invitees.

Most of the work is in the clients: key creation and storage, the pinned list and the
comparison step, the envelope, sealing and opening, verdicts, and the vectors.

### Order of work, in items of about 45 minutes

1. **ikigai-layers:** `seal:` terms in `vocabulary.ttl`, a shipped sealed shape and context, a
   test that a sealed channel admits a `seal:Sealed` entry and refuses a plaintext one.
2. **ikigai-layers:** the `sealed = true` option and its refusals at open, `layers:sealed` in the
   description, and the README section.
3. **Vectors:** the fixture set of section 7 (identities, signing keys, envelopes, signatures,
   sealed entries, refusals), in the agreed home, with the CCTV age vectors wired into CI.
4. **Rust:** the envelope (build, parse, the signing bytes) and its vectors, in ikigai-layers'
   participant build (no host dependencies), sealing and opening through ikigai-encrypt.
5. **Rust:** opening with verdicts (signature, sender, counter, `seen`, recipient set, inner
   shapes), with each refusal vector.
6. **Rust cli:** key creation and storage with mode checks, the pinned list, and the
   fingerprint comparison step.
7. **Rust cli:** `say` and reading on a sealed layer, with the lines of section 7.
8. **Python:** pyrage, the envelope and opening, passing the same vectors.
9. **Python client:** keys, the pinned list, comparison, `say`, and the plain and JSON lines.
10. **gonk:** the Layers page shows sealed entries as the "sealed entry" line.

### What not to build

- Server-side anything on plaintext: moderation hooks, search, `match=` over sealed tuples, a
  host-held "recovery" key, or key escrow. Each is the operator reading by another name.
- Keys in `participants.ttl` or manifests as a source of trust.
- Plaintext fallback when sealing fails.
- History re-sharing, dynamic membership and in-band key rotation, until the fixed version has
  been used.
- Sealed public feeds.
- A hand-written age implementation in Python.
- Forward secrecy or post-compromise security with age; if those become requirements, the
  answer is a group protocol (MLS), a different design.
