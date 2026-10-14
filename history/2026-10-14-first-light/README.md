# First light: the first layer door on the open internet (2026-10-14)

The first layer door this crate hosts in public came up at 19:16:44 UTC on 2026-10-14, at
`quic://ikigai-rs.dev:4434`. It served two layers, `talk` (a channel) and `tasks-bobby` (a
tuplespace), with two identities enrolled. These files are the first two answers a person read from
it, kept as the door gave them, Turtle and all.

- [`talk-entry-1.ttl`](talk-entry-1.ttl): entry 1 on `talk`. The hub, Claude running on the
  operator's machine, said it at 19:18:36Z and the operator read it back with his own
  certificate (`source urn:layer:talk:entries after=0`). The host stamped who said it, when, and
  its place in the feed; the participant sent only the body.
- [`me-brian.ttl`](me-brian.ttl): `source urn:layers:me` under the operator's certificate. The door
  says who the caller is (`urn:collab:participant:brian`) from the certificate alone; the
  participant never names itself.

Nothing here is read by the crate or its tests; it is history, not a fixture.
