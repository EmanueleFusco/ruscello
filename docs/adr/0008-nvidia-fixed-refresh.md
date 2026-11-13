# ADR-0008 — Optional NVIDIA fixed refresh for the desktop app

- Status: Accepted (2026-11-13)

## Context

Ruscello's default hardware renderer uses FIFO presentation. On the owner's Windows and NVIDIA setup,
windowed G-SYNC still caused flicker with FIFO. Setting fixed refresh for Ruscello in NVIDIA App resolved
the reported flicker while preserving hardware rendering.

## Decision

Expose an opt-in control in General settings on Windows. Use the NVIDIA Driver Settings API (DRS) to read
the profile matched to the running executable and apply the fixed-refresh value for its G-SYNC override.
The option leaves the renderer selection independent and applies after restarting the desktop app.

## Alternatives

- Continue using the manual per-program setting in NVIDIA App. This remains available for externally
  managed and shared profiles.
- Select the existing software renderer. This remains available as the compatibility option in ADR-0001.
- Override the global refresh policy or render continuously. Either approach affects behavior beyond
  the affected app profile or increases idle work.

## Consequences

- As a narrow system-driver integration under ADR-0007, the desktop loads the installed NVIDIA
  driver interface from Windows System32. It ships no NVIDIA binaries. The FFI binding uses the public MIT-licensed
  [NVAPI headers](https://github.com/NVIDIA/nvapi) for layouts and interface IDs.
- Checking is read-only. An explicit enable or restore action is scoped to a profile with one
  application. An existing fixed-refresh setting remains externally managed. Shared profiles and
  changes to the managed value outside Ruscello require management in NVIDIA App.
- Ruscello records the previous local value before committing the override. Restoration reinstates that
  value, or removes the local override to recover inheritance. It removes an app-created profile when
  fixed refresh is its only setting.
- The blocking driver calls run outside the GUI thread. Non-Windows builds omit the integration.
  Windows systems without the driver can still use either renderer.
