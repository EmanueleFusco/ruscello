# ADR-0013 — Share the download bandwidth budget across protocols

## Context

The scheduler previously applied the effective download cap independently to HTTP and the BitTorrent session.
Both could individually reach that cap while running together. Torrent requests also have optional session and
per-torrent caps, which are useful even when the global limit is disabled.

## Decision

Use the existing adjustable download token bucket as the engine-wide budget. The BitTorrent wrapper passes
an optional budget callback into the vendored session. After honoring BitTorrent's own limits, a peer acquires
the bytes for each block request before sending it. HTTP, FTP and native media keep using the same bucket for
received payload. The scheduler changes the single rate when applying normal, scheduled or alternate limits.
Existing torrent caps continue to apply to their original scope.
Grant requests wait in arrival order at a limited global bucket. A request waiting for a category
or per-download bucket releases its global turn while it waits for that scope.

## Alternatives

Splitting the limit by active protocol would waste capacity when either protocol stalls and would require periodic
rate reassignment. Separate limits would change the meaning of the existing global setting. Charging a torrent
after bytes arrive would fail to bound useful in-flight requests when the cap is low.

## Consequences

The wires can briefly exceed the displayed rate while requests already issued under an earlier cap finish.
Peer requests already paid for remain eligible after a cap change. Cancelled or retransmitted requests may have
spent capacity without contributing verified bytes. Changes to the cap do not recreate the torrent session or
rewrite saved settings. The callback and its cancellation behavior need review when updating librqbit.
