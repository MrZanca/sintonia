# M5, fase 0: prova di fattibilità Android

Esito: **la strada «Slint + mpv» regge. Si resta su mpv dentro Slint; ExoPlayer non serve.**
Resta aperto un problema di licenza (libmpv LGPL per Android), non tecnico: vedi «Rischi».

## Cosa è stato provato

Un prototipo fuori dal workspace (`prototipi/android-fase0`, non fa parte dell'app) con:

- una finestra Slint (FemtoVG, backend `android-activity`, `NativeActivity`) con due pulsanti, una
  zona video e una riga di statistiche;
- il vero `sintonia-player` (nessuna modifica: `build.rs` per Android cade già nel ramo che collega
  `mpv`, e la cartella delle `.so` si passa dal `build.rs` del prototipo) con `RenderContext` creato
  dentro il rendering notifier, la texture OpenGL ES resa come immagine Slint, come in `video.rs`;
- `scripts/android-prova.ps1`: compila con `cargo-ndk`, trasforma le classi Java di Slint in
  `classes.dex` (d8), impacchetta con `aapt2`, allinea (`zipalign`) e firma con la chiave di debug.

Prove sul Xiaomi Mi 9T (Snapdragon 730, Adreno 618, OpenGL ES 3.2, Android 11, arm64-v8a),
flusso HLS di prova pubblico `test-streams.mux.dev` (H.264 1920×1080 a 60 fps):

| Prova | Esito |
|---|---|
| Interfaccia Slint, testo, tocco (`TouchArea`) | funziona; due tocchi su «Tocchi» → contatore 2 |
| mpv nel rendering notifier, texture → `Image` Slint | funziona, video con le bande nere giuste |
| `hwdec=auto-safe` | sceglie **`mediacodec-copy`** |
| `hwdec=mediacodec` (senza copia) | funziona (`hwdec-current=mediacodec`) |
| `hwdec=no` (software) | funziona, ma costa il doppio |
| Audio | `audiotrack`, parte e resta sincronizzato (avsync ≈ 0) |
| Rotazione | il video si ridimensiona e continua a 60 fps |
| App in secondo piano e ritorno | riprende da sola a 60 fps (nessun errore) |
| Cambio modalità `hwdec` a caldo (pulsante) | ricarica e riparte |

## Numeri misurati

| Misura | Valore |
|---|---|
| Avvio a freddo (`am start -W`, TotalTime) | 392–682 ms (media circa 500 ms) |
| Dal processo al contesto video di mpv | 221 ms |
| Dal comando di caricamento al primo fotogramma | 3,5 s (dipende dalla rete e dall'HLS, non dal telefono) |
| Fotogrammi video / interfaccia al secondo | **60 / 60** in tutte le modalità; intervallo massimo 17–33 ms |
| Fotogrammi persi dal decoder | 0 |
| Fotogrammi persi dall'uscita video (`vo`) | 0 a regime; 27–41 nei primi secondi dopo l'avvio o dopo il cambio di modalità; circa 20 in più a ogni rotazione |
| CPU (media su 10 s, % di un core) | `mediacodec` 57%, `mediacodec-copy` 70%, software 127% |
| Memoria residente | 240–310 MB |
| Dimensione dell'APK di prova | 36 MB (FFmpeg 12 MB + libmpv 6,5 MB + prova 13 MB senza `strip`) |
| Temperatura batteria | 30 °C dopo alcuni minuti |

Non è stato fatto il confronto con ExoPlayer: non serve, perché mpv rispetta già l'obiettivo (60 fps
reali con la decodifica hardware, nessuna perdita a regime).

## Scelta

**mpv dentro Slint.** Motivi: i numeri sopra, `sintonia-player` si riusa intero (la fase 2 si riduce
a `build.rs`, ciclo di vita e opzioni Android), e tutto il resto dell'app (riconnessione, tracce,
ripresa) resta uguale a Windows. ExoPlayer avrebbe imposto di riscrivere il player e una `SurfaceView`
sotto Slint.

## Cosa è emerso e va portato nelle fasi successive

1. **Stato OpenGL.** Su Android FemtoVG non reimposta da sé tutto ciò che mpv tocca. Il prototipo
   salva e ripristina (`SavedGl`) framebuffer, viewport, texture legate, programma, VAO, buffer,
   flag (blend, scissor, stencil, depth, cull), maschere, `blend_func`, stencil e depth, scissor e
   colore di cancellazione. Nella prima versione, che ripristinava solo framebuffer e viewport e senza
   video in corso, il testo usciva come blocchi neri; non ho isolato quale delle due correzioni (stato
   completo o dimensioni esplicite, punto 2) lo risolva, quindi si applicano entrambe. Il codice
   di `video.rs` salva solo framebuffer e viewport: in fase 2 va portato lo stesso salvataggio
   completo (dietro `cfg(target_os = "android")`, o per tutti se non costa).
2. **Dimensioni dell'immagine.** La texture è in pixel fisici: se l'`Image` Slint non ha larghezza e
   altezza esplicite, la sua dimensione propria (in pixel logici) fa crescere il riquadro a ogni
   fotogramma sugli schermi con scala > 1 (qui 2,75). Con `width`/`height` espliciti è a posto.
   Controllare `VideoView` in `app.slint`.
3. **`av_jni_set_java_vm`.** FFmpeg ha bisogno della JavaVM per `mediacodec` e per l'audio: va
   chiamata all'avvio con `app.vm_as_ptr()`. Bisogna collegare `libavcodec` (lo fa `build.rs`).
4. **Schermo spento / secondo piano.** Slint smette di disegnare (0 fotogrammi) ma mpv continua a
   decodificare e conta circa 40 fotogrammi persi al secondo. Non c'è stato un teardown del
   rendering. La fase 2 deve mettere in pausa (video) o fermare (diretta) alla perdita del focus e
   riprendere al ritorno. Il ritorno dal secondo piano ha funzionato; **il ritorno dallo schermo
   spento con blocco impronta non è stato provato** (serviva sbloccare il telefono).
5. **Tabella dei font.** Slint/FemtoVG stampa avvisi di Fontconfig su Android ma disegna il testo
   con i font di sistema (Roboto): nessun problema visibile.
6. **Backend Slint per Android.** Il suo `build.rs` compila un aiuto in Java (`javac` e `android.jar`)
   e l'APK deve contenere il relativo `classes.dex` (script: `d8`). Serve JDK 17 e
   `ANDROID_JAR`/`ANDROID_HOME`. La `NativeActivity` basta per la prova; per la tastiera su schermo
   (fase 4) valutare `GameActivity` (richiede Gradle).
7. **Bordi dello schermo.** Il titolo finisce sotto la barra di stato (nessuna gestione degli
   inset): fase 4.
8. **Installazione sul Xiaomi.** `adb install` è bloccato da MIUI finché non si attiva «Installa via
   USB» nelle Opzioni sviluppatore (serve account Mi e dati mobili) e si sblocca il telefono.

## Rischi rimasti

- **libmpv LGPL per Android: non esiste una build pronta.** L'unica trovata
  (`jarnedemeulemeester/libmpv-android` v1.0.0, derivata da `mpv-android`) ha FFmpeg compilato con
  `--enable-gpl --enable-version3` e mpv con la licenza predefinita (GPL). Sintonia è GPL-3.0, quindi
  per le misure e per i test interni non è un ostacolo, ma la regola del progetto (libmpv LGPL)
  vieta di distribuirla. Strada prevista: ripetere le `buildscripts` di `mpv-android` su Linux
  (GitHub Actions: qui non ci sono WSL né Docker) togliendo `--enable-gpl` da `ffmpeg.sh` e
  aggiungendo `-Dgpl=false` a `mpv.sh`, poi controllare che le dipendenze restino compatibili
  (dav1d BSD, mbedtls Apache-2.0, libxml2 MIT, libass ISC, freetype FTL, harfbuzz MIT, fribidi LGPL).
  **Non provato.** È il primo lavoro della fase 2 e può chiedere aggiustamenti (mpv ≥ 0.40 con
  `gpl=false` esclude alcune funzioni).
- La libmpv trovata è la API client 2.5 (`sintonia-player` accetta qualunque 2.x). Una build propria
  sarà probabilmente più recente: ritestare `render.rs`.
- **Non provati:** HEVC e AV1, 10 bit/HDR, flussi MPEG-TS in diretta (il flusso di prova è HLS),
  riproduzione per ore, consumo di batteria, schermo spento con blocco, chiamata in arrivo, cambio
  di rete, dispositivi vecchi o a 32 bit, emulatore x86_64 (le librerie ci sono, la prova non è stata
  fatta), Android TV.
- Dimensione dell'APK: FFmpeg da solo pesa 12 MB per ABI; con più ABI si può superare i 60 MB.
  Per le Release di GitHub conviene un APK per ABI (o solo arm64-v8a).
- Il consumo di CPU con `mediacodec` (57% di un core su 8) è alto per una sola riproduzione: in parte
  è FemtoVG che ridisegna l'intera interfaccia a 60 fps. Da ottimizzare (ridisegno solo delle zone
  cambiate, video a risoluzione dello schermo) prima della fase 6.

## Impostazioni consigliate per la fase 2

`hwdec=mediacodec,mediacodec-copy` con ripiego su `no`; `ao=audiotrack`; `RenderContext` rilasciato e
ricreato alla perdita di superficie; salvataggio completo dello stato OpenGL attorno a mpv.

## Verifiche

- Windows: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features`, `cargo test`
  eseguiti dopo le modifiche (vedi il rapporto della sessione); il codice dell'app non è stato toccato.
- Il prototipo non rispetta i lint del workspace (nessun `forbid` per `unwrap`, ma sì i commenti
  `// SAFETY:`); è un esperimento da buttare via, non codice di produzione.
