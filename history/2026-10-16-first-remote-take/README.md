# The first remote take (2026-10-16)

On the evening of the [first message from outside](../2026-10-16-first-external-message/), work crossed the public
layer door in the other direction. A ledger on one machine offered an item into the task layer `tasks-bobby`, and a
gonk on the collaborator's own laptop took it, filed it into his ledger, and started reporting back. Nobody moved a
message by hand.

The sequence, every step done by a machine under its own enrolled certificate:

1. **00:48:59Z.** `urn:collab:participant:plasma-gonk`, the operator's gonk, offered two items from its `bobby`
   ledger into `tasks-bobby`. Each offer carried only a title, a brief and a repository; ledger internals do not
   cross.
2. **00:52:37Z.** `urn:collab:participant:bobby-gonk`, a gonk on the collaborator's laptop, took the offer
   `urn:layer:tasks-bobby:offer:01m53nar1efba0bwefhhnmrw9g` ("Set up on WSL2: build ikigai and run the ikigai Book's
   first chapters"). The take is atomic, and the host wrote a receipt naming the taker before either side heard back.
3. The offering gonk saw the take in the layer's feed and claimed the item in its own ledger for the taker, with a
   seven-day lease.
4. **00:52:38Z.** The taker's gonk dropped its first `Progress` report. The offering gonk took that report and renewed
   the lease.

- [`tasks-bobby-taken.txt`](tasks-bobby-taken.txt): the layer's record of the two takes, as the door gave it to the
  operator's certificate (`source urn:layer:tasks-bobby:tuples state=taken`). Each line gives the feed position, the
  time, the tuple and the participant that took it. Line 3 is the collaborator's gonk taking the offer; line 5 is ours
  taking his Progress report.
- [`remote-gonk.jpg`](remote-gonk.jpg): the collaborator's gonk at `localhost:1062` on his laptop, a Windows machine
  running gonk under WSL2. Its `default` ledger shows one open item, #1, the taken offer filed as his own work, on
  the road at 7:54 PM local time. (Metadata stripped; resized from the original.)

It took one fix to get here: a binding's feed long poll had shared a connection with its other calls, so every pass
behind it timed out (ikigai-gonk PR 184). Both gonks ran that fix the same evening.

Nothing here is read by the crate or its tests; it is history, not a fixture.
