# Strategia di test

"Stabile e testato" è un requisito esplicito: nessuna feature è completa senza test automatici.

## Livelli

| Livello | Dove | Strumenti | Note |
|---------|------|-----------|------|
| Unit | in ogni crate (`#[cfg(test)]`) | `cargo test`, `proptest` | Allocatore segmenti, state machine, parser (Content-Disposition, m3u8, ricette), punteggio LinkGrabber |
| Property | `ruscello-http`, `ruscello-core` | `proptest` | Invarianti: copertura ranges, nessuna sovrapposizione, monotonia progressi, transizioni di stato valide |
| Integrazione | `crates/*/tests/` | `ruscello-testkit`, `tempfile`, `tokio::test` | Download reali contro server locale con scenari di guasto |
| Crash/resume | `apps/cli/tests/` | sottoprocesso `ruscello-cli` + kill | Kill in momenti casuali (seed riproducibile) → riavvio → SHA-256 corretto. **Obbligatorio in CI** |
| Snapshot | resolver, GUI | `insta` | Output del LinkGrabber su fixture HTML; viste iced |
| GUI | `apps/gui/tests/` | `iced_test` | Flussi: aggiungi, pausa, riprendi, annulla, filtri sidebar |
| Torrent | `crates/torrent/tests/` | seeder+tracker locali | Multi-file, interruzione, ricontrollo hash |
| Fuzz | `fuzz/` | `cargo-fuzz` | Parser esposti a input remoti (non in CI per PR; job notturno) |
| Benchmark | `benches/`, `scripts/bench*` | `criterion`, confronto con aria2 | Budget di `PLAN.md` §2 |
| Live | dietro feature `live-tests` | rete reale | Plugin hoster/siti reali; esecuzione manuale o job schedulato non bloccante |

## Testkit (`ruscello-testkit`)

Server HTTP locale in-process (porta effimera) con builder di scenari:

```rust
// indicativo
let srv = TestServer::start().await;
let url = srv.file(FileSpec::random(seed, 256 * MIB)
    .ranges(true)
    .etag("v1")
    .drop_after_bytes(10 * MIB)        // la connessione cade
    .throttle_per_conn(2 * MIB)        // B/s
    .max_conns_per_ip(4)               // oltre: 503
    .latency(Duration::from_millis(50)));
```

Anche pagine HTML finte che replicano i pattern degli hoster (fuckingfast-like: link `/dl/...` nello
script; XFileSharing-like: form `method_free` → countdown → `downloadbtn`; pagina di "verifica" finta) per
testare plugin, ricette e browser helper **senza rete reale**.

## Fixture

- File binari generati da seed (mai committati). HTML reali salvati in `crates/resolver/tests/fixtures/`
  (piccoli, ripuliti da dati personali), con data di acquisizione nel nome.

## Gate CI (ogni PR)

1. `cargo fmt --all --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace` su `windows-latest`, `ubuntu-latest`, `macos-latest`
4. `cargo deny check`
5. (da M10) job notturno: fuzz breve, soak ridotto, benchmark con soglie.

## Regole

- Test deterministici: niente `sleep` arbitrari; usare clock iniettabile o attese su eventi con timeout.
- Ogni bug corretto → test di regressione.
- I test non scrivono fuori da `tempfile::TempDir`.
