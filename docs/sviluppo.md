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
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test

# prova tutti i canali di una playlist (stampa solo nomi ed esiti, mai gli indirizzi)
cargo run --release -p sintonia-player --example probe -- lista.m3u 15 8

# zip portatile per Windows in dist\
powershell -ExecutionPolicy Bypass -File scripts\pacchetto.ps1
```

Log: `RUST_LOG=debug` (o `info,sintonia=debug,mpv=info`). I messaggi di mpv passano per
`sintonia_core::redact`: gli URL vengono ridotti a `schema://host/…`, così utente e password dei
fornitori IPTV non finiscono mai nei log.

Finto server Xtream Codes (dati inventati, flussi di prova pubblici), per provare film, serie e
preferiti senza un account vero:

```powershell
cargo run -p sintonia-core --features mock --example finto_xtream   # http://127.0.0.1:8899
# nell'app: Impostazioni › Xtream Codes, server http://127.0.0.1:8899, utente mario, password prova123
cargo run -p sintonia-core --features mock --example finto_xtream -- 8899 20000 30000 2000   # catalogo grande
```

Ispezione dell'interfaccia con il server MCP di Slint (Slint ≥ 1.17):

```powershell
$env:SLINT_EMIT_DEBUG_INFO = 1; $env:SLINT_MCP_PORT = 9315
cargo run -p sintonia-app --features slint/mcp
```

## Struttura

| Crate | Ruolo |
|---|---|
| `sintonia-core` | logica senza interfaccia: `m3u` (parser tollerante a righe), `xtream` (client `player_api.php`, risposte lette con tolleranza) e `xtream_mock` (finto server, solo test ed esempio), `db` (SQLite + FTS5: canali, film, serie, preferiti, cronologia), `crypto` e `secrets` (indirizzi cifrati, chiave e password Xtream nel portachiavi), `service` (runtime tokio: download, archivio, immagini), `redact` |
| `sintonia-player` | involucro di libmpv: `Player` (comandi, tracce, opzioni per canale, eventi da un thread dedicato), `Backoff` (riconnessione), `RenderContext` (render API OpenGL) |
| `sintonia-app` | finestre Slint: `app.rs` (collega interfaccia, servizio e player; ripresa, prossimo episodio), `catalog.rs` (modello virtualizzato per canali, film e serie, con preferiti), `images.rs` (locandine ritagliate con gli angoli di `Sq`), `video.rs` (video di mpv nelle finestre, riconnessione, fine dei film), `settings.rs` |

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

### Dati e sicurezza (M1)

- Archivio: `%APPDATA%\Sintonia\data\archivio.db` (SQLite). Cache dei loghi: `%LOCALAPPDATA%\Sintonia\cache\loghi`
  (file chiamati con l'hash dell'indirizzo, mai con l'indirizzo).
- Gli indirizzi della playlist, della guida e di **ogni canale** sono cifrati (XChaCha20-Poly1305).
  La chiave sta nella Gestione credenziali di Windows (servizio `Sintonia`, voce `chiave-archivio`).
  Se la chiave sparisce, all'avvio l'archivio vecchio viene messo da parte e se ne crea uno nuovo.
- L'interfaccia non fa I/O: tutto passa dal servizio (`Command` → `Event`).

### Xtream, preferiti e ripresa (M2)

- La password Xtream non va mai nell'archivio: sta nel portachiavi (voce `sorgente-<id>`) e viene
  cancellata insieme alla sorgente. Server e utente sono cifrati come gli indirizzi.
- Ogni contenuto ha una **chiave stabile** (`l<id>`/`m<id>`/`s<id>` per Xtream, hash dell'indirizzo
  per M3U): preferiti, cronologia e «ultimo canale» sopravvivono all'aggiornamento della sorgente.
- Film ed episodi salvano il punto ogni 10 s e all'uscita; oltre il 95% (o a meno di un minuto dalla
  fine) sono «visti»: il film esce da «Riprendi», la serie passa all'episodio successivo.
- `VideoRouter::play` toglie sempre la pausa: in mpv la pausa vale per il core, non per il file.

### Traduzioni

- Testi in inglese con `@tr()` in `ui/app.slint`; italiano in `lang/it/LC_MESSAGES/sintonia-app.po`.
- Compilate dentro l'eseguibile, senza contesto (`DefaultTranslationContext::None`); cambio immediato con
  `slint::select_bundled_translation`. Nuova lingua: copiare il `.po` in `lang/<codice>/…` e tradurre.
- Il test `tests/translations.rs` fallisce se un testo non ha la traduzione italiana.

### Trabocchetti di Slint incontrati

- Con una traduzione attiva, **la conversione numero → testo usa il separatore decimale della lingua**
  (in italiano la virgola). I percorsi SVG costruiti concatenando numeri (`Sq`) si rompono: per questo
  `Sq` lavora con interi in centesimi di pixel.
- `row` è una proprietà predefinita degli elementi (griglie): non usarla come nome di proprietà.
- Non si può chiamare una funzione o leggere un `id` di un elemento dentro un `if`: per passare il
  focus a una pagina condizionale si usa una proprietà contatore con `changed`.
- Un `Path` con `stroke-width` viene adattato al riquadro meno il bordo e, con `fit: contain`
  (il valore normale), **mantenendo le proporzioni**: le forme larghe e basse si restringono di
  circa 2·larghezza/altezza pixel. `Sq` usa `fit: fill`.
- Un elemento a larghezza fissa dentro un `VerticalLayout` ne limita la larghezza massima: va messo in
  un suo `HorizontalLayout { alignment: start; }`.
- Elenchi lunghi: `ChannelList` disegna solo le righe visibili (`for i in visibili` + `model[indice]`),
  come `ListView` ma con l'aspetto della bozza.