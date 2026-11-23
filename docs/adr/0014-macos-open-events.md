# ADR-0014 — Deliver macOS open requests to the desktop import flow

## Context

Launch Services delivers file and URL activation as Apple events. The existing desktop startup argument and single-instance paths cover Windows and Linux. Iced and Winit own the macOS application event loop and its AppKit delegate.

## Decision

Declare `magnet`, `.torrent`, and `.dlc` in the macOS application bundle. Register Foundation handlers for the two Launch Services open events in the primary process, including during AppKit's will-finish-launching notification. Send normalized batches through the same input channel that serves the desktop's authenticated local IPC. Keep the document roles as alternate viewers and leave default-handler selection to the operating system and user.

## Alternatives

Replacing the application delegate would interfere with Winit's event handling. Forwarding only command-line arguments would depend on a launcher translating the events. The native handler uses the existing Objective-C bindings and a small set of documented selectors.

## Consequences

Both cold and warm activation feed the existing import UI. The installed bundle must be tested on macOS for Launch Services registration, delivery timing and default-handler behavior. The native bridge builds only for Apple targets.
