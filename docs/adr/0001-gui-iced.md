# ADR-0001 — GUI con iced

- Stato: Accettato (2026-11-12)

## Contesto
Requisito: tutto in Rust, interfaccia professionale e minimale, efficiente (CPU ~0 a riposo), Windows e
Linux prioritari, macOS compatibile, progetto GPL.

## Decisione
Usare **iced ≥ 0.14** (Elm architecture, rendering reattivo, widget `table`, `iced_test` per test headless,
licenza MIT). Scelta confermata dal committente.

## Alternative
- **Slint**: DSL dichiarativo con anteprima live, molto rifinito; licenza GPLv3/commerciale e linguaggio
  separato dal Rust.
- **egui**: immediate mode, semplice ma aspetto meno "professionale" e ridisegni più frequenti.
- **Tauri/Dioxus desktop**: frontend web/webview, contrario a "tutto Rust" per la UI e più pesante.

## Conseguenze
- Tema e stile custom vanno scritti in Rust (modulo `theme` in `apps/gui`).
- Lista virtualizzata per migliaia di righe: usare `table`/`lazy`/scrollable con rendering delle sole righe
  visibili; se insufficiente, widget custom.
- Il loop di iced (winit) occupa il main thread → webview in processo separato (ADR-0004).
