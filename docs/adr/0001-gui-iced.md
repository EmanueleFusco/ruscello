# ADR-0001 — GUI Framework: iced

- Status: Accepted (2026-11-12)

## Context
Requirements: 100% Rust codebase (UI + backend), professional and minimal interface, resource-efficient (~0% CPU when
idle), Windows and Linux as equal primary targets with macOS compatibility, GPL project license, and bilingual UI
(English + Italian).

## Decision
Use **iced ≥ 0.14** (Elm architecture, reactive rendering, native `table` widget, `iced_test` for headless UI testing,
permissive MIT license). Confirmed with the project owner.

## Alternatives Considered
- **Slint**: declarative `.slint` DSL with live preview and high visual polish; dual-licensed (GPLv3 / commercial) and
  introduces a separate non-Rust markup language.
- **egui**: immediate-mode GUI; simpler for quick tools, but harder to style as a calm desktop app and prone to more
  frequent redraws.
- **Tauri / Dioxus desktop**: web/webview frontend, violating the pure-Rust UI requirement and increasing RAM overhead.

## Consequences
- Custom themes and widget styles are implemented directly in Rust (`theme` module in `apps/gui`).
- Virtualized download list for thousands of rows: use `table`/`lazy`/`scrollable` rendering only visible rows, or a
  custom widget if needed.
- `iced` (`winit`) owns the main thread event loop → the browser helper webview runs in a separate process (ADR-0004).
- **Dual Renderer Backend (`tiny-skia` + `wgpu`) & G-SYNC / OLED Compatibility:**
  - On Windows systems with NVIDIA GPUs, G-SYNC Windowed Mode, and OLED monitors, initializing a raw DirectX 12 /
    Vulkan / OpenGL 3D swapchain (`wgpu`) causes the NVIDIA driver to classify the desktop app as a 3D game (triggering
    the NVIDIA App overlay popup, hooking G-SYNC to an event-driven variable-FPS window, causing OLED VRR gamma
    flickering, and inducing DWM desktop stutter).
  - Therefore, `ruscello-gui` must compile `iced` with **both** `wgpu` (GPU hardware renderer) and `tiny-skia` (CPU 2D
    software renderer) enabled, and expose a user-selectable **UI Renderer** setting in *Settings → General / Advanced*:
    - **`Hardware GPU (wgpu)` (Default):** standard `iced` hardware-accelerated renderer (using VSync FIFO presentation
      mode).
    - **`Software 2D (tiny-skia)` (Compatibility / G-SYNC & OLED mode):** sets `ICED_BACKEND=tiny-skia` in `main()`
      before initializing `iced`. Never initializes a DX12/Vulkan/OpenGL 3D swapchain, completely preventing NVIDIA App
      game detection, G-SYNC OLED flickering, and DWM lag on affected configurations.
