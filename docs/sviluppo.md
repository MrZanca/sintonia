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
Su PC senza driver video (alcune macchine virtuali) la DLL non si carica: l'installer lo controlla e avvisa.

## Android (M5)

Ambiente su Windows, già installato in `%LOCALAPPDATA%\Android`: `platform-tools` (adb, nel PATH),
`jdk17`, `sdk` (platform 34, build-tools 34.0.0, NDK 27.2.12479018), `cargo-ndk` 4.1.2 e i target
Rust `aarch64-linux-android` e `x86_64-linux-android`. In ogni shell:

```powershell
$a = "$env:LOCALAPPDATA\Android"
$env:ANDROID_HOME = "$a\sdk"; $env:ANDROID_NDK_HOME = "$a\sdk\ndk\27.2.12479018"; $env:JAVA_HOME = "$a\jdk17"
cargo ndk -t arm64-v8a build -p sintonia-core                        # il core compila per Android
cargo ndk -t arm64-v8a clippy -p sintonia-player --all-targets -- -D warnings   # il player (serve libmpv, sotto)
```

Il backend Android di Slint compila un aiuto Java: serve `ANDROID_JAR` (lo imposta lo script).

### libmpv LGPL per Android

Sta in `vendor\mpv-android\<abi>\` (non va nel repository): `libmpv.so`, le librerie di FFmpeg
(`libav*.so`, `libsw*.so`) e `libc++_shared.so`, più `LICENSES\` e `BUILDINFO.txt` (versioni,
configurazioni, SHA-256 di ogni `.so`). Un'altra cartella si indica con `SINTONIA_MPV_DIR` (con dentro
le cartelle per ABI). Il `build.rs` di `sintonia-player` la collega e passa il percorso ai crate che
ne dipendono come `DEP_MPV_LIB_DIR`: le `.so` vanno nell'APK in `lib/<abi>/`.

Non esiste una build LGPL pronta: la produce il workflow **libmpv Android**
(`.github/workflows/libmpv-android.yml`, su Ubuntu 24.04) con `scripts/libmpv-android.sh`, che ripete
le `buildscripts` di mpv-android a commit fissato togliendo `--enable-gpl` da FFmpeg e aggiungendo
`-Dgpl=false` a mpv, e si ferma se la licenza risultante non è LGPL (vedi `THIRD-PARTY.md`). Il
workflow parte quando cambiano lo script o il workflow stesso, o a mano (solo con il file su `main`);
dura circa 13 minuti per arm64-v8a e x86_64. Lo script gira anche su un Linux qualsiasi
(`bash scripts/libmpv-android.sh`, con SDK e NDK scaricati dallo script di mpv-android).

Versione in uso: esecuzione `41205587384` del 31/10/2026, mpv `6f605372b9` (API client 2.6, come
Windows), FFmpeg n9.0.2, NDK r30, API minima 23. SHA-256 di `libmpv-android-lgpl.zip`
`1f3bd5fab46aeda25bf8032cb41000febfeffe74800a93a16dd4e9a553cc0dd5`. Per installarla (scarica con `gh`,
controlla lo SHA-256 e sposta un'eventuale build vecchia in `vendor\mpv-android-gpl\`):

```powershell
powershell -ExecutionPolicy Bypass -File scripts\android-libmpv.ps1 -Run 41205587384 -Sha256 1f3bd5fab46aeda25bf8032cb41000febfeffe74800a93a16dd4e9a553cc0dd5
```

Gli artefatti delle esecuzioni di GitHub scadono dopo 90 giorni: dopo, si rilancia il workflow e si
aggiornano qui esecuzione e SHA-256 (le `.so` non sono identiche bit per bit tra due compilazioni).

La build **GPL** della fase 0 (`jarnedemeulemeester/libmpv-android` v1.0.0, SHA-256
`df146592480fc8418415a06b1f1a1d6318b0088e21f52254b0e9a82b61ca8fa2`) non serve più e non va mai distribuita.

### Opzioni del player su Android

`hwdec=mediacodec,mediacodec-copy` (se nessuno dei due accetta il flusso mpv decodifica in software),
`ao=audiotrack`. All'avvio l'app deve chiamare `sintonia_player::set_java_vm(app.vm_as_ptr())` e passare
in `PlayerConfig::tls_ca_file` il file fatto da `sintonia_player::tls::system_ca_bundle(cartella)`:
mpv verifica i certificati HTTPS, e libcurl e FFmpeg (mbedtls) su Android non trovano da soli quelli di
sistema («TLS certificate verification failed»). Il ciclo di vita si decide con
`sintonia_player::lifecycle::Lifecycle` (segnali `Start`/`Stop`/`InitWindow`/`TerminateWindow` di
`slint::android::init_with_event_listener`) e si applica con `VideoRouter::suspend`/`resume`.

### Banco di prova (prototipi\android-fase0)

```powershell
# compila, impacchetta, firma (chiave di debug) e installa
powershell -ExecutionPolicy Bypass -File scripts\android-prova.ps1 -Installa   # -Abi x86_64 per l'emulatore

# prova.txt nella cartella dati: prima riga l'indirizzo; poi, se servono, hwdec=..., live=no (film:
# pausa invece di fermata in secondo piano), opt=nome=valore (un'opzione di mpv)
adb shell "run-as it.sintonia.prova sh -c 'echo URL > files/prova.txt && echo live=no >> files/prova.txt'"
adb shell am start -n it.sintonia.prova/android.app.NativeActivity
adb logcat -s sintonia:*          # una riga di numeri al secondo, il ciclo di vita, mai l'indirizzo
```

Un indirizzo con utente e password non va mai scritto sulla riga di comando (finisce nella cronologia
della shell): si scrive in un file senza BOM, `adb push` in `/data/local/tmp`, poi `run-as ... cat` e si
cancella. Con una pipe di Windows PowerShell 5.1 verso `adb shell` l'indirizzo arriva con un BOM davanti
e mpv non lo apre.

CPU per processo: `adb shell top -b -d 1 -n 11 -p (adb shell pidof it.sintonia.prova)`; per thread `-H`.

### Il core su Android (fase 1)

```powershell
# compila tutto il core per l'emulatore (x86_64) e controlla i lint sul codice Android
cargo ndk -t x86_64 build -p sintonia-core
cargo ndk -t arm64-v8a clippy -p sintonia-core --all-targets --all-features -- -D warnings

# test del core sul telefono: compila senza lanciare, toglie i simboli, copia ed esegue
# (senza TMPDIR e dalla cartella /, come dentro un'app; il percorso del binario lo stampa cargo)
cargo ndk -t arm64-v8a test -p sintonia-core --no-run
& "$env:ANDROID_NDK_HOME\toolchains\llvm\prebuilt\windows-x86_64\bin\llvm-strip.exe" -o core-test target\aarch64-linux-android\debug\deps\sintonia_core-<hash>
adb push core-test /data/local/tmp/sintonia-core-test
adb shell "chmod 755 /data/local/tmp/sintonia-core-test; cd / && env -u TMPDIR /data/local/tmp/sintonia-core-test"

# prova dentro un'app vera (JVM, Keystore, cartelle dell'app, HTTPS): prototipi\android-fase1
powershell -ExecutionPolicy Bypass -File scripts\android-core-prova.ps1 -Installa
adb shell am start -W -n it.sintonia.provacore/android.app.NativeActivity
adb logcat -s sintonia-prova:*                            # il rapporto, fino a «FINE»
adb shell run-as it.sintonia.provacore cat files/prova-core.txt   # lo stesso rapporto su file
```

Su Android l'app deve chiamare `sintonia_core::android::init(app.vm_as_ptr(), app.activity_as_ptr())`
all'avvio, prima di segreti, cartelle (`sintonia_core::android::app_dirs()`) e client HTTP.

In Git Bash `adb push`/`adb shell` storpiano i percorsi che iniziano con `/`: usare
`MSYS_NO_PATHCONV=1` o PowerShell.

**Xiaomi (MIUI/HyperOS):** `adb install` fallisce con `INSTALL_FAILED_USER_RESTRICTED` finché non si
attiva Opzioni sviluppatore › «Installa via USB» (serve l'account Mi con i dati mobili) e il
telefono è sbloccato. In alternativa: `adb push` dell'APK in `/sdcard/Download` e installazione a mano
dal gestore file. Il telefono con schermo spento e blocco impronta rifiuta anche i tocchi di `adb input`.

## Comandi

```powershell
cargo run -p sintonia-app              # avvia l'app (la DLL viene copiata accanto all'eseguibile)
cargo fmt --all --check
cargo clippy --all-targets --all-features
cargo test

# prova tutti i canali di una playlist (stampa solo nomi ed esiti, mai gli indirizzi)
cargo run --release -p sintonia-player --example probe -- lista.m3u 15 8

# legge una guida XMLTV (anche .gz) e conta i canali abbinati a una playlist (solo conteggi)
cargo run --release -p sintonia-core --example leggi_guida -- guida.xml.gz lista.m3u

# zip portatile per Windows in dist\
powershell -ExecutionPolicy Bypass -File scripts\pacchetto.ps1          # -Prova: aggiunge finto_xtream.exe
# prova di carico del servizio con dati inventati (playlist 50 000 canali, guida 500 000 programmi,
# Xtream grande, file rotti, chiusure forzate, due processi sullo stesso archivio)
cargo run --release -p sintonia-core --features mock --example stress -- 50000 500000

# app con un profilo di prova: archivio, cache e portachiavi a parte (non tocca i dati veri)
$env:SINTONIA_PROFILO = "prova"; cargo run -p sintonia-app

# anche l'installer (serve Inno Setup 6: https://jrsoftware.org/isinfo.php)
powershell -ExecutionPolicy Bypass -File scripts\pacchetto.ps1 -Installer
```

Log: `RUST_LOG=debug` (o `info,sintonia=debug,mpv=info`). I messaggi di mpv passano per
`sintonia_core::redact`: gli URL vengono ridotti a `schema://host/…`, così utente e password dei
fornitori IPTV non finiscono mai nei log.

Finto server Xtream Codes (dati inventati, flussi di prova pubblici), per provare film, serie e
preferiti senza un account vero:

```powershell
cargo run -p sintonia-core --features mock --example finto_xtream   # http://127.0.0.1:8899
# nell'app: Impostazioni › Xtream Codes, server http://127.0.0.1:8899, utente mario, password prova123
# il finto server ha anche la guida (xmltv.php), con programmi inventati attorno all'ora attuale
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
| `sintonia-core` | logica senza interfaccia: `m3u` (parser tollerante a righe), `epg` (XMLTV a flusso, anche `.gz`, e abbinamento ai canali), `xtream` (client `player_api.php`, risposte lette con tolleranza) e `xtream_mock` (finto server, solo test ed esempio), `db` (SQLite + FTS5: canali, film, serie, preferiti, cronologia, guida), `crypto` e `secrets` (indirizzi cifrati, chiave e password Xtream nel portachiavi), `service` (runtime tokio: download, archivio, immagini), `redact` |
| `sintonia-player` | involucro di libmpv: `Player` (comandi, tracce, opzioni per canale, eventi da un thread dedicato), `Backoff` (riconnessione), `RenderContext` (render API OpenGL), `lifecycle` (sospensione su Android), `tls` (autorità di certificazione per Android) |
| `sintonia-app` | finestre Slint: `app.rs` (collega interfaccia, servizio e player; ripresa, prossimo episodio), `catalog.rs` (modello virtualizzato per canali, film e serie, con preferiti), `guide.rs` (palinsesto in memoria, «in onda / dopo», griglia della Guida), `images.rs` (locandine ritagliate con gli angoli di `Sq`), `video.rs` (video di mpv nelle finestre, riconnessione, fine dei film), `settings.rs` |

Il logo sta in `assets/logo/` (SVG d'origine, PNG da 32 a 1024 px, `sintonia.ico`): l'icona
dell'eseguibile entra da `crates/sintonia-app/sintonia.rc` (build script, `embed-resource`), quella
delle finestre e il logo della barra in alto da `ui/app.slint`.

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

### Guida dei programmi (M3)

- Da dove: la guida scelta per la sorgente (Impostazioni › Sorgenti › Guida, indirizzo o file),
  altrimenti `xmltv.php` per Xtream o `url-tvg`/`x-tvg-url` della playlist. Gli indirizzi sono
  cifrati come gli altri; quello Xtream contiene la password e non si salva mai.
- Lettura a flusso con `quick-xml` (gzip riconosciuto dai primi byte), importazione a blocchi di
  5000 programmi in righe «in attesa» (`pending = 1`): la guida vecchia resta visibile finché la
  nuova non è completa, e un'importazione interrotta si scarta all'avvio. Si tengono i programmi
  da ieri a +8 giorni.
- Abbinamento (`epg::Matcher`, tabella `epg_map`): `tvg-id` uguale, poi identificativo o nome
  normalizzati («Tele 1 HD», «Tele.1.HD..101.it» e «Tele1.it» diventano «tele1»); a parità vincono i
  canali della guida con programmi. Si rifà a ogni aggiornamento della playlist o della guida.
- Aggiornamento: dopo il salvataggio o l'aggiornamento di una sorgente, e all'avvio se la guida ha
  più di 12 ore (impostazione «Aggiorna la guida all'avvio»).
- L'interfaccia chiede finestre di programmi (`LoadGuide`): una attorno ad adesso per «in onda /
  dopo» e una per il giorno della Guida; la griglia disegna solo le righe e i programmi visibili.

### Impostazioni, PIN e build automatiche (M4)

- **Aggiornamento automatico** (Impostazioni › Aggiornamenti): playlist e guida della sorgente in uso si
  riscaricano quando sono più vecchie dell'intervallo scelto (`playlist_hours`, predefinito 24 ore;
  `epg_hours`, predefinito 12; 0 = mai). Il servizio controlla all'avvio, quando cambia la sorgente e
  ogni 10 minuti; dopo un errore riprova dopo un'ora. Gli errori automatici vanno solo nel log.
- **PIN** (Impostazioni › Controllo genitori): da 4 a 8 cifre, salvato come impronta SHA-256 con sale
  (`sintonia_core::pin`), mai in chiaro. Le categorie bloccate (per sorgente, chiave `locks.<id>`)
  finché il PIN non è inserito restano negli elenchi con il lucchetto ma le loro voci spariscono da
  «Tutti», preferiti, ricerca, Home, ripresa e ultimo canale (`CatalogModel::set_locks`). Il PIN vale
  fino alla chiusura dell'app; «Blocca ora» lo richiude. Protegge dai bambini, non da chi può leggere
  i file del PC.
- **Installer**: `installer/sintonia.iss` (Inno Setup 6), installazione per l'utente senza diritti di
  amministratore (o per tutti, a scelta), collegamenti nel menu Start e sul desktop, disinstallazione
  che lascia i dati in `%APPDATA%\Sintonia`. Avvisa se manca `vulkan-1.dll`.
- **GitHub Actions** (`.github/workflows/windows.yml`): a ogni push su `main` e a ogni pull request
  scarica libmpv LGPL (controllando lo SHA-256), ricava `mpv.lib`, esegue fmt, clippy e test, crea zip e
  installer e li allega all'esecuzione. Con un tag `vX.Y.Z` uguale alla versione di `Cargo.toml`
  pubblica anche la Release. Per fare una versione: `git tag v0.4.0` e `git push origin v0.4.0`.

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