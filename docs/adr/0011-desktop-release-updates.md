# ADR-0011 — Desktop package updates

- Status: Accepted (2026-11-14)

## Context

The desktop application ships with a sibling native browser helper. Updating only the GUI could leave a
different protocol implementation installed. An update also must respect the ordered durability guarantees
of the download writer before replacing running binaries.

## Decision

Use the official GitHub Releases API for stable release discovery. Compare versions by SemVer precedence,
select the exact package name for the installed format and architecture, and require GitHub's SHA-256 digest
and size. Trust the official repository over HTTPS and the restricted GitHub asset redirect hosts. Stage
and verify all bytes before requesting graceful engine shutdown. The engine cancellation path waits for
writers, reports checkpoint failures, and closes storage. The primary desktop instance applies the verified
update after the GUI exits. Active transfers are eligible to resume on the next startup.

- Windows uses a per-user NSIS installer with silent handoff and relaunch. It stages both executables and
  retains the previous files until replacement succeeds.
- Linux replaces a writable AppImage with a same-filesystem rename. Debian packages are installed through
  the system package manager.
- macOS copies the complete app bundle out of the verified DMG and swaps it into the original location.
  Replacement attempts to restore the previous bundle if the second rename fails.

## Alternatives

A standalone executable replacement library would leave the sibling helper on its old version. A separately
signed manifest could improve trust isolation, at the cost of provisioning and rotating a signing key.
Debian updates through the package manager preserve its installation records and permission boundaries.

## Consequences

The GitHub repository's release workflow is part of the update trust boundary. Development and mismatched
packages can discover releases and link to the release page. Privileged installations require manual package
updates. macOS distribution uses ad hoc signing until Developer ID and notarization credentials are available.
Tests can serve metadata and assets using a local fixture without enabling other production endpoints.
