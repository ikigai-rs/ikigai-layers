# Mentoring layers

A participant posts a question and receives answers in the same channel. To request a
review, they also drop a keyed offer for a specific artifact revision. A mentor, whether a
person or an enrolled agent, takes the offer and gets a receipt for the assignment.
A designated approver can then endorse an exact answer so readers can cite it with its
approval record.

## Existing behavior

The current [channel write path](../../src/endpoints.rs) stamps and stores one entry at a
host-minted IRI. It indexes `ik:inReplyTo` as [entry metadata](../../src/channel.rs).
The write path validates the submitted entry graph, so even an IRI with the right layer
prefix can point to a missing entry or the wrong type. A receipt's settlement checks
that `result=` is an IRI. Its target is also resolved by the reader. The
[tuplespace tests](../../tests/tuplespace.rs) exercise competing takes and receipt lookup.
The [channel tests](../../tests/channel.rs) exercise exact entry lookup, restart
persistence, and reply metadata. These primitives leave approval authority and
cross-layer verification to the design below.

## Layer definitions

The following blocks specify the proposed files under a deployment's `layers/`
directory. The example names describe one mentoring community. Deployments should
allocate collision-free layer names and substitute them consistently in file names,
grants, and IRI patterns before accepting writes. The `m:` vocabulary is local to these
definitions. Layer names and classes are singular. Tuple subjects use a type-first
local name followed by a key. Channels use the host's fixed `entry:{id}` form and carry
the specific type in `rdf:type`.

### Question channel

`layers/question.toml`

```toml
model = "channel"
kind = "talk"
shapes = "question.shapes.ttl"
context = "question.context.jsonld"
```

`layers/question.shapes.ttl`

```turtle
@prefix m: <urn:layers:mentoring:> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

m:QuestionShape a sh:NodeShape ;
    sh:targetClass m:Question ;
    sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:Question ; sh:maxCount 1 ],
        [ sh:path dcterms:title ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ; sh:minLength 1 ; sh:maxLength 160 ],
        [ sh:path ik:body ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ; sh:minLength 1 ; sh:maxLength 4000 ],
        [ sh:path prov:wasAttributedTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path dcterms:created ; sh:datatype xsd:dateTime ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:seq ; sh:datatype xsd:integer ; sh:minCount 1 ; sh:maxCount 1 ] .

m:AnswerShape a sh:NodeShape ;
    sh:targetClass m:Answer ;
    sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:Answer ; sh:maxCount 1 ],
        [ sh:path ik:body ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ; sh:minLength 1 ; sh:maxLength 4000 ],
        [ sh:path ik:inReplyTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ;
          sh:pattern "^urn:layer:question:entry:[0-9a-hjkmnp-tv-z]{23}$" ],
        [ sh:path m:receipt ; sh:nodeKind sh:IRI ; sh:maxCount 1 ;
          sh:pattern "^urn:layer:review-offer:receipt:[0-9a-hjkmnp-tv-z]{23}$" ],
        [ sh:path prov:wasAttributedTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path dcterms:created ; sh:datatype xsd:dateTime ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:seq ; sh:datatype xsd:integer ; sh:minCount 1 ; sh:maxCount 1 ] .
```

`layers/question.context.jsonld`

```json
{
  "@context": {
    "Question": "urn:layers:mentoring:Question",
    "Answer": "urn:layers:mentoring:Answer",
    "title": "http://purl.org/dc/terms/title",
    "body": "https://ikigai-rs.dev/ns#body",
    "inReplyTo": { "@id": "https://ikigai-rs.dev/ns#inReplyTo", "@type": "@id" },
    "receipt": { "@id": "urn:layers:mentoring:receipt", "@type": "@id" }
  }
}
```

### Review-offer tuplespace

`layers/review-offer.toml`

```toml
model = "tuplespace"
kind = "task"
shapes = "review-offer.shapes.ttl"
context = "review-offer.context.jsonld"
lease_max = "1h"
lease_required = true
```

`layers/review-offer.shapes.ttl`

```turtle
@prefix m: <urn:layers:mentoring:> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

m:ReviewOfferShape a sh:NodeShape ;
    sh:targetClass m:ReviewOffer ;
    sh:closed true ;
    sh:pattern "^urn:layer:review-offer:offer:[0-7][0-9a-hjkmnp-tv-z]{25}$" ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:ReviewOffer ; sh:maxCount 1 ],
        [ sh:path m:question ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ;
          sh:pattern "^urn:layer:question:entry:[0-9a-hjkmnp-tv-z]{23}$" ],
        [ sh:path m:artifact ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:brief ; sh:datatype xsd:string ; sh:minCount 1 ; sh:maxCount 1 ; sh:minLength 1 ; sh:maxLength 2000 ],
        [ sh:path prov:wasAttributedTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path dcterms:created ; sh:datatype xsd:dateTime ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:seq ; sh:datatype xsd:integer ; sh:minCount 1 ; sh:maxCount 1 ] .
```

`layers/review-offer.context.jsonld`

```json
{
  "@context": {
    "ReviewOffer": "urn:layers:mentoring:ReviewOffer",
    "question": { "@id": "urn:layers:mentoring:question", "@type": "@id" },
    "artifact": { "@id": "urn:layers:mentoring:artifact", "@type": "@id" },
    "brief": "https://ikigai-rs.dev/ns#brief"
  }
}
```

### Approval channel

`layers/answer-approval.toml`

```toml
model = "channel"
shapes = "answer-approval.shapes.ttl"
context = "answer-approval.context.jsonld"
about = "urn:layers:mentoring:answer"
```

`layers/answer-approval.shapes.ttl`

```turtle
@prefix m: <urn:layers:mentoring:> .
@prefix ik: <https://ikigai-rs.dev/ns#> .
@prefix dcterms: <http://purl.org/dc/terms/> .
@prefix prov: <http://www.w3.org/ns/prov#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

m:AnswerApprovalShape a sh:NodeShape ;
    sh:targetClass m:AnswerApproval ;
    sh:closed true ;
    sh:property
        [ sh:path rdf:type ; sh:hasValue m:AnswerApproval ; sh:maxCount 1 ],
        [ sh:path m:answer ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ;
          sh:pattern "^urn:layer:question:entry:[0-9a-hjkmnp-tv-z]{23}$" ],
        [ sh:path prov:wasAttributedTo ; sh:nodeKind sh:IRI ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path dcterms:created ; sh:datatype xsd:dateTime ; sh:minCount 1 ; sh:maxCount 1 ],
        [ sh:path ik:seq ; sh:datatype xsd:integer ; sh:minCount 1 ; sh:maxCount 1 ] .
```

`layers/answer-approval.context.jsonld`

```json
{
  "@context": {
    "AnswerApproval": "urn:layers:mentoring:AnswerApproval",
    "answer": { "@id": "urn:layers:mentoring:answer", "@type": "@id" }
  }
}
```

The host adds its stamp and receipt terms to each egress context. The closed shapes
admit the three write stamps because validation runs after stamping. Clients submit
channel entries with the placeholder subject `urn:layer:new` and retain the IRI
returned by `say`. They send tuple subjects with a lowercase ULID they allocate once
per offer, then reuse the same subject and graph when retrying a lost drop reply.

## Grants and workflow

These are ordinary [collaboration mounts](../../README.md#configuration). Each column
lists the grants for an enrolled principal in that role. An account serving several
roles holds the union. Deployments grant `say` on `answer-approval` exclusively to
designated approver principals and review the effective grants across all manifests.

| Layer | Participant | Mentor | Approver |
| --- | --- | --- | --- |
| `question` | `read`, `feed`, `say` | `read`, `feed`, `say` | `read`, `feed` |
| `review-offer` | `read`, `feed`, `out` | `read`, `feed`, `take` | `read` |
| `answer-approval` | `read`, `feed` | `read`, `feed` | `read`, `feed`, `say` |

1. The asker posts a `Question`. For a review, they drop a `ReviewOffer` with that
   question's IRI, an artifact IRI fixed to the revision to be reviewed, and the review
   request. Artifact access stays with the artifact's owner. Layer read grants expose
   entries or receipts to the layer's readers, so a community should include only
   content and links suitable for that audience.
2. A mentor takes the exact offer through `Delete urn:layer:review-offer:tuples`
   with `tuple=` and a lease, for example `lease=30m`. The host commits the receipt,
   stamps the taker and offer, and returns the tuple. The mentor reads
   `Source urn:layer:review-offer:tuples state=taken tuple=<offer-IRI>` to get the
   receipt IRI and checks the taker and lease state. After a lapse, the offer is
   available again and the next attempt has its own receipt. The layer refuses a
   take without a lease and leaves the offer pending.
3. The mentor posts an `Answer` with `ik:inReplyTo` pointing to the question and
   `m:receipt` pointing to that attempt. They settle the receipt before the lease lapses
   with a Sink on the receipt IRI using `settle=done result=<answer-IRI>`. Ordinary answers omit the receipt and
   require no take. The two review writes are separate. After a lost `say` reply,
   recover the committed answer from the question channel's feed and inspect its
   author and receipt before retrying. A lost settle reply can be retried with the
   same result. A lapsed attempt's answer stays in the channel, so a reader should
   check the receipt before treating that entry as the completed review.
4. An approver reads the exact answer and verifies that its reply target exists and
   is a `Question`. For a review answer, they also verify that the receipt names the
   answer's author as taker, that its embedded offer refers to the same question,
   and that its completed lease names this exact answer IRI as the result. They check
   that the answer addresses the question or the requested artifact revision and is
   suitable for citation within the audience. An approver acts independently of the
   answer author. This check is a policy for approvers holding `say`, since channel
   grants do not compare an approver's identity with another entry's author.
5. The approver posts an `AnswerApproval` naming the exact answer IRI. The host stamps
   who approved it, when, and in what sequence. Each admitted approval is an
   endorsement by a principal authorized to write to that channel at the time.
   Readers resolve its target and apply the same checks for a valid question
   and, when present, a completed review. They can discover approval with
   `Source urn:layer:answer-approval:latest about=<answer-IRI>` or follow the approval
   feed. Repeated endorsements can coexist. This workflow allows multiple approved
   answers to a question.

## Citation and retention

A citation uses the answer's existing `urn:layer:question:entry:{id}` and can include
the approval entry's IRI as evidence. Approval gives the answer an endorsement while
preserving its content and identity. Resolve both with `Source` through the originating
host and the corresponding `read` grants. These are host-scoped identifiers. A citation
shared across hosts needs the originating door or another stable host locator alongside
the IRI. Sharing a citation grants no additional access.

Keep the question and approval channel state and the review receipts that support
approval. The review-offer definition leaves `retain` unset. Preserve layer names, state,
and any state encryption key across restarts, backups, and migrations. Channels keep
immutable entries in the saved state. A correction gets a new answer IRI and its own
approval, while earlier citations resolve to the earlier text and endorsement. This
design records historical endorsement and has no withdrawal operation.

## Verification

On base commit `003690a6a80d98457d92520709af477e464867dd`,
`cargo test --test channel --test tuplespace --test lease` passed 55 tests.
A temporary local test loaded the original nine definition blocks into the existing
host fixture, before the review-offer layer could require a lease. It confirmed
that a well-formed reply to a missing question is stored,
then exercised a valid question, leased take and receipt lookup, answer and
settlement, grant denial for an unauthorized approver, approval lookup by answer,
and JSON-LD reads by the original citation IRIs after reopening the host.
The repository's regular tests cover the underlying host behavior. The mentoring
definitions remain a design and are available here for review.

## Proposed follow-up items

- **Validate mentoring references on write.** Define reusable admission checks for an
  existing question, a completed receipt and its result, and independent approval.
  Exercise missing and wrong-type targets, a mismatched taker, and late settlement.
- **Record approval withdrawal.** Design an append-only decision that keeps each
  citation resolvable and makes a withdrawn endorsement visible to readers.
- **Resolve citations across host moves.** Specify a durable host locator and test
  restoring an answer, its approval, and a supporting receipt under their original
  IRIs and read grants.
