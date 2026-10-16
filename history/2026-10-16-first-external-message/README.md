# The first message from outside (2026-10-16)

Two days after [first light](../2026-10-14-first-light/), the public layer door at
`quic://ikigai-rs.dev:4434` carried its first message from someone outside the operator's own
machines. At 22:22:15 UTC on 2026-10-16, `urn:collab:participant:bobby`, the first collaborator
enrolled on the door, said entry 4 on `talk` from his own machine, with a certificate he generated
there. His private key never left that machine. The door had his public certificate, enrolled by the
SHA-256 fingerprint of its DER. Both sides read that fingerprint aloud on a call before it went in,
and the door's own `server.crt` fingerprint the same way.

- [`talk-entry-4.ttl`](talk-entry-4.ttl): the entry as the door gave it to the operator's
  certificate (`source urn:layer:talk:entries after=3`). As with entry 1, the participant sent only
  the body. The host stamped who said it (`prov:wasAttributedTo`), when (`dcterms:created`) and
  its place in the feed (`ik:seq`), from the certificate on the connection.

The body is a sealed prediction: "A magical prediction for a moment in the future when the time is
right", followed by a 34-byte multihash in hex. Its first byte is `0x12` (SHA2-256) and its second
is `0x20` (32 bytes), followed by the digest. Until the text is revealed, the entry proves only
that its author committed to something at this time, on an append-only channel that nothing edits
or deletes. Once the text is revealed, anyone can check it against the digest.

Nothing here is read by the crate or its tests; it is history, not a fixture.
