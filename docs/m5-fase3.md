# M5, fase 3: l'applicazione gira su Android

Esito: **Sintonia gira sul telefono con la stessa interfaccia di Windows.** Ramo `m5-fase-3`
(partito da `main` dopo l'unione delle fasi 0-2). Importazione di una playlist da file e di un account
Xtream, guida, riproduzione con decodifica hardware e riavvio con i dati ancora presenti sono provati
sul Mi 9T. È scomoda come previsto: il tocco, i bordi e le dimensioni per il telefono sono la fase 4.

## Cosa è cambiato

| Parte | Cosa |
|---|---|
| `sintonia-app` | Diventa una libreria (`lib.rs`, tutto quello che c'era in `main.rs`) con un `main.rs` di tre righe per il PC. Su Android l'APK carica la libreria come `cdylib` (`cargo rustc --crate-type cdylib`, solo nello script: sul PC il `Cargo.toml` non la dichiara, così Windows non compila una DLL in più). |
| Punto d'ingresso | `src/android.rs`: `android_main` inizializza log (`android_logger`), core (`sintonia_core::android::init`), JavaVM per mpv, metodi nativi dell'activity e backend Android di Slint, poi chiama lo stesso `run()` del PC. |
| Slint | Su Android la dipendenza è separata: solo `backend-android-activity-06` + FemtoVG, perché il backend winit di Slint su Android non compila. Il codice che usa winit (`remote.rs` per i tasti speciali, `zoom.rs`, il puntatore nascosto) sta dietro `cfg(not(target_os = "android"))`. |
| Activity | `android/java/.../SintoniaActivity.java`: una `NativeActivity` con poche righe di Java, compilata con `javac` + `d8` insieme all'aiuto Java di Slint (niente Gradle). Fa quello che da Rust non si raggiunge: selettore di file (Storage Access Framework, il risultato arriva solo a `onActivityResult`), browser (`Intent.ACTION_VIEW`), messaggi (`AlertDialog`), schermo acceso (`FLAG_KEEP_SCREEN_ON`), tutto schermo immersivo. |
| Selettore di file | Il documento scelto viene copiato in `files/playlist/<nome>` (il core legge file con un percorso; un permesso su `content://` non sopravvive al riavvio), poi Rust riceve il percorso con un metodo nativo registrato all'avvio (`RegisterNatives`). Reimportare lo stesso nome aggiorna la copia. |
| Ciclo di vita | I segnali `Start`/`Stop`/`InitWindow`/`TerminateWindow` passano da `sintonia_player::lifecycle::Lifecycle` a `VideoRouter::suspend`/`resume` (fase 2). |
| Mini player | Non esiste: su Android i componenti Slint condividono l'unica finestra e si vede l'ultimo creato, quindi `PipWindow` si crea per primo, si collega e si lascia andare. I pulsanti e il tasto del mini player sono nascosti (`Nav.mini-player`). |
| Scala | L'interfaccia è disegnata per 1320 × 800 punti; con la scala di Android (2,75 sul Mi 9T) non ci starebbe. `android::fit_design` sceglie la scala che la fa stare intera (1,35 in orizzontale, 0,82 in verticale) e la ricontrolla ogni mezzo secondo. Provvisorio: la fase 4 la sostituisce. |
| Tutto schermo | Barra di stato e di navigazione nascoste (modalità immersiva): la barra di stato copriva i pulsanti in alto (Impostazioni non si poteva toccare). I margini veri sono la fase 4. |
| `mediakeys.rs` | Resta l'interfaccia, inutilizzata: su Android i tasti del volume restano al sistema. |
| `sintonia-player` | Spenti gli script Lua interni di mpv (`load-stats-overlay`, `load-console`, `load-select`, `load-positioning`, `load-commands`, `load-context-menu`, `load-auto-profiles`): Sintonia non li usa. Uno dei due problemi lasciati aperti dalla fase 2. |
| APK | `scripts/android-app.ps1`: libreria, classi Java, manifest (`io.github.mrzanca.sintonia`, versione da `Cargo.toml`), libmpv LGPL, firma con la chiave di debug. 57 MB per arm64-v8a. |

## Tre blocchi trovati e corretti

1. **Schermo vuoto:** si vedeva il mini player (sfondo con un alone al centro) al posto della finestra
   principale, perché era il componente creato per ultimo nell'unica finestra di Android.
2. **App ferma all'avvio con la ripresa del canale:** `AndroidApp::set_window_flags` chiamato prima che
   la finestra esista bloccava il thread dell'app. Lo schermo acceso ora passa dall'activity Java.
3. **Log muti:** il filtro di `android_logger` (`info,mpv=warn`) scartava tutto; resta solo il livello.

## Prove (Xiaomi Mi 9T, Android 11, arm64-v8a, Wi-Fi)

- Avvio: interfaccia completa in italiano, OpenGL ES 3.2 (Adreno 618).
- Tocco: pulsanti, schede, campi di testo; la **tastiera su schermo funziona già** con `NativeActivity`
  (la porta l'aiuto Java di Slint): GameActivity, e quindi Gradle, non serve.
- Xtream: finto server del core (`finto_xtream` sul PC, `adb reverse tcp:8899 tcp:8899`): 40 canali, 60
  film, 12 serie, guida da 3047 programmi; la password va nel deposito cifrato con il Keystore (fase 1).
- Importazione da file: selettore di Android → `files/playlist/prova-sintonia.m3u` → sorgente da 2
  canali. La scelta del file l'ha fatta l'utente a mano: MIUI non accetta i tocchi simulati con adb sui
  file del selettore. Anche l'annullamento riporta all'app senza errori.
- Guida: griglia dei programmi completa.
- Riproduzione: flusso HLS 1280×720 in diretta, `h264_mediacodec`, anteprima nella Home e player a
  tutto schermo; CPU circa 33% di un core a tutto schermo, 14% con il solo audio; memoria circa 195 MB.
- Riavvio (`am force-stop` e riapertura): sorgenti, «Riprendi» e ultimo canale ritrovati; il video
  riparte da solo.
- Ciclo di vita: selettore di file, schermo spento e ritorno → `Suspend` e `Resume` senza errori.
- Windows: `cargo fmt --all --check`, `cargo clippy --all-targets --all-features`, `cargo test`
  puliti; clippy per Android (`-D warnings`) pulito per app, player e core.

## Non provato o rimasto

- Emulatore x86_64 e Android TV; orientamento verticale solo di passaggio (scala 0,82, molto piccola).
- L'avviso «nuova versione» che apre il browser e il messaggio di archivio danneggiato: il codice c'è,
  ma non si sono presentati i casi.
- Riproduzione per tempi lunghi, film e serie (on demand).

## Per la fase 4

- Il titolo e i pulsanti in alto vanno tenuti lontani dai bordi (ora c'è il tutto schermo immersivo).
- La tastiera mette la maiuscola iniziale negli indirizzi («Http://») e copre i campi in basso.
- I pallini della password escono come quadratini: il carattere manca nei font di Android
  (`Bahnschrift` non c'è, Slint ripiega su Roboto).
- Interfaccia pensata per il telefono al posto della scala provvisoria; tasto Indietro di Android.
- Prima che il flusso parta il riquadro del video mostra solo il numero del canale.
