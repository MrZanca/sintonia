# Sviluppo di Sintonia

## Cosa serve (Windows)

- Rust stabile con toolchain `x86_64-pc-windows-msvc` (provato con 1.99.1).
- Visual Studio Build Tools 2022 con il carico di lavoro C++ (per il linker MSVC e `lib.exe`).
- **libmpv in versione LGPL** in `vendor/mpv/` (la cartella non va nel repository):
  `libmpv-2.dll`, `mpv.lib` e `include/mpv/*.h`. In alternativa si indica un'altra cartella con
  la variabile `SINTONIA_MPV_DIR`.

### Preparare libmpv

1. Scaricare `mpv-dev-lgpl-x86_64-AAAAMMGG-git-<commit>.7z` dalle release di
   [zhongfly/mpv-winbuild](https://github.com/zhongfly/mpv-winbuild/releases) e confrontarne lo
   SHA-256 con `sha256.txt` della stessa release.
   La build LGPL è prodotta da `compile-lgpl-libmpv.patch`: mpv con `-Dgpl=false`, FFmpeg senza
   `--enable-gpl` (quindi LGPLv3) e senza x264, x265, rubberband, dvdnav, srt.
   Va usata la variante `x86_64`, non `x86_64-v3`, che richiede CPU Haswell o successive.
2. Estrarla in `vendor/mpv/`.
3. Il pacchetto contiene `libmpv.dll.a` (per MinGW) ma non la libreria d'importazione per MSVC.
   Si ricava dalle esportazioni della DLL, da un *Developer PowerShell for VS 2022*:

   ```powershell
   cd vendor\mpv
   $names = dumpbin /nologo /exports libmpv-2.dll |
     ForEach-Object { if ($_ -match '^\s+\d+\s+[0-9A-F]+\s+[0-9A-F]{8}\s+(\S+)') { $Matches[1] } }
   Set-Content mpv.def (@('LIBRARY libmpv-2.dll', 'EXPORTS') + ($names | ForEach-Object { "    $_" })) -Encoding ascii
   lib /nologo /def:mpv.def /machine:x64 /out:mpv.lib
   ```

Versione in uso: release `2026-10-17-6f605372b9`, mpv commit `6f605372b9`, API client 2.6,
SHA-256 dell'archivio `b7a12a73a03313446657d3bf1bda525fa8b62c2628669ee0a16c0430f0a312c9`.

`libmpv-2.dll` dipende da `vulkan-1.dll`, che installano i driver video (NVIDIA, AMD, Intel).
Su PC senza driver video (alcune macchine virtuali) la DLL non si carica: da gestire nell'installer (M4).

## Comandi

```powershell
cargo run -p sintonia-app              # avvia l'app (la DLL viene copiata accanto all'eseguibile)
cargo run -p sintonia-app -- <url>     # M0: prova un proprio flusso su tutti i canali
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test
```

Log: `RUST_LOG=debug` (o `info,sintonia=debug,mpv=info`). I messaggi di mpv passano per
`sintonia_core::redact`: gli URL vengono ridotti a `schema://host/…`, così utente e password dei
fornitori IPTV non finiscono mai nei log.

Ispezione dell'interfaccia con il server MCP di Slint (Slint ≥ 1.17):

```powershell
$env:SLINT_EMIT_DEBUG_INFO = 1; $env:SLINT_MCP_PORT = 9315
cargo run -p sintonia-app --features slint/mcp
```

## Struttura

| Crate | Ruolo |
|---|---|
| `sintonia-core` | logica senza interfaccia; per ora solo `redact` (M1-M3 aggiungono M3U, Xtream, EPG, SQLite) |
| `sintonia-player` | involucro di libmpv: `Player` (comandi ed eventi da un thread dedicato) e `RenderContext` (render API OpenGL) |
| `sintonia-app` | finestre Slint; `video.rs` porta il video di mpv nelle finestre |

### Il video dentro Slint (M0)

- Slint usa il renderer **FemtoVG su OpenGL** (`BackendSelector::require_opengl`).
- Nel *rendering notifier* di ogni finestra, prima che Slint disegni, mpv disegna il fotogramma in
  una texture grande quanto il riquadro `VideoView` (pixel fisici). Uno shader rende trasparenti
  gli angoli con la stessa super-ellisse del componente `Sq`. La texture diventa un'immagine Slint
  (`Video.frame`): i controlli stanno sopra e gli angoli restano esatti su qualsiasi sfondo.
- **Mini player**: mpv ammette un solo contesto di disegno per core, legato a un contesto OpenGL,
  e Slint non crea contesti condivisi tra finestre. Il contesto quindi *si sposta*: la finestra
  che lo ha lo libera nel suo prossimo fotogramma, quella di destinazione lo ricrea, e
  `Player::reselect_video` riaccende la traccia video. Il flusso di rete resta uno solo (molti
  fornitori ammettono una connessione sola) e la decodifica hardware resta attiva.
  Il prezzo è un attimo di nero durante il passaggio: sui flussi live, sotto il secondo.
