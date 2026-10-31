# M5: Sintonia per Android

Stato di partenza: versione 0.5.0 per Windows. Tre crate: `sintonia-core` (logica, archivio SQLite,
liste M3U/Xtream, guida), `sintonia-player` (involucro di libmpv con la render API OpenGL) e
`sintonia-app` (finestra Slint con renderer FemtoVG e backend winit).

## Cosa si riusa e cosa cambia

| Parte | Su Android |
|---|---|
| `sintonia-core` | Si riusa quasi intero. Da rivedere: `keyring` (non ha un backend Android), `directories` (percorsi), `rusqlite` con `bundled` e `reqwest`/rustls (servono la toolchain NDK). |
| `sintonia-player` | `build.rs` collega oggi libmpv solo su Windows. Serve una libmpv LGPL per Android (`libmpv.so`) per arm64-v8a. |
| Video in Slint | La texture OpenGL di mpv dentro Slint funziona con un contesto OpenGL ES. Va riprovata sul backend Android di Slint. |
| Interfaccia | Il codice per il telecomando (`Nav.tv`) si riusa per Android TV. Il touch va aggiunto. |
| Pezzi solo Windows | `awake.rs`, `mediakeys.rs`, apertura del browser, selettore file (`rfd`): vanno rifatti con le API Android (JNI). |

## Decisioni prese

1. **Dove gira**: smartphone, tablet e Android TV. Il piano prevede prima telefono e tablet e poi
   la TV (fase 4).
2. **Dispositivo di prova**: un telefono vero, collegato al PC con il cavo USB (debug USB, vedi
   sotto). Per la TV si userà l'emulatore Android TV, salvo un box o una TV disponibile.
3. **Distribuzione**: solo APK scaricabile dalle Release di GitHub, niente F-Droid né Google Play.
4. **Firma**: la chiave di firma la crea e la custodisce l'utente (fase 5); non va mai nel
   repository. Fino alla fase 4 si usa la chiave di debug che Android crea da sola.

## Fasi

### Fase 0: prova di fattibilità (nessuna modifica all'app)
Obiettivo: sapere se la strada «Slint + mpv» regge su Android prima di costruirci sopra.
- Ambiente di build su Windows: Android Studio (SDK, NDK r27 o più recente), `rustup target add
  aarch64-linux-android`, `cargo-ndk`. Scrivere i comandi in `docs/sviluppo.md`.
- App minima Slint (una finestra con un pulsante) con il backend Android (`android-activity`),
  installata sul dispositivo.
- libmpv LGPL per Android: partire dalle build di `mpv-android` (script `buildscripts`) o da una
  libmpv già compilata con licenza compatibile; verificare la licenza (niente GPL, niente
  x264/x265) e annotare versione e SHA-256 come già fatto per Windows.
- Provare la render API di mpv su OpenGL ES dentro il rendering notifier di Slint con un flusso di
  prova. Misurare: avvio, fotogrammi persi, decodifica hardware (`mediacodec`).
- **Punto di decisione**: se mpv dentro Slint non funziona o è troppo lento, l'alternativa è
  ExoPlayer/Media3 con una `SurfaceView` sotto l'interfaccia, pilotata via JNI. Riscrive il
  `sintonia-player` per Android. Si decide qui, con i numeri in mano, e si scrive in questo file.
- Consegna: `docs/m5-fase0.md` con esito, numeri e scelta.

### Fase 1: il core gira su Android
- Compilare `sintonia-core` per `aarch64-linux-android` (e `x86_64-linux-android` per
  l'emulatore); far passare i test che girano su dispositivo (`cargo-ndk` + `adb`).
- Percorsi: cartella dati dell'app (`internal_data_path` di `android-activity`) al posto di
  `directories`; cache nella cartella cache.
- Segreti (`secrets.rs`): sostituire `keyring` su Android con una chiave nell'Android Keystore (JNI)
  che protegge i dati con la cifratura già presente in `crypto.rs`. Stessa interfaccia, nessun
  cambio per il resto del codice.
- Rete: certificati di sistema con rustls (`rustls-platform-verifier`), DNS, proxy.
- `PRAGMA integrity_check` e istanza unica (`single_instance`) vanno rivisti: su Android l'app è
  una sola per definizione, e il file di blocco va nella cartella dati.

### Fase 2: il video
- `sintonia-player/build.rs`: ramo Android che collega `libmpv.so` da una cartella indicata da
  `SINTONIA_MPV_DIR` (come per Windows); la `.so` finisce in `jniLibs/<abi>/` dell'APK.
- Contesto OpenGL ES, `hwdec=mediacodec` (con ripiego su `mediacodec-copy`), uscita audio
  (`audiotrack` o `opensles`).
- Ciclo di vita: quando l'app va in secondo piano Android distrugge la superficie e il contesto
  OpenGL. Il `RenderContext` va rilasciato e ricreato, e la riproduzione ripresa o messa in pausa
  (video: pausa; canale in diretta: ferma e riparte al ritorno).
- Il mini player (seconda finestra) non esiste su Android: lo sostituisce la modalità
  Picture-in-Picture, in fase 5.

### Fase 3: l'applicazione
- Punto d'ingresso `android_main` (`#[no_mangle]`), `cfg(target_os = "android")` per `main.rs`.
- Selettore file (importare una playlist M3U): Storage Access Framework via JNI al posto di `rfd`.
- Aprire una pagina nel browser (avviso nuova versione): `Intent.ACTION_VIEW` via JNI.
- Schermo sempre acceso durante il video: `FLAG_KEEP_SCREEN_ON` al posto di `awake.rs`.
- `mediakeys.rs`: su Android i tasti multimediali arrivano come tasti normali; il resto resta vuoto.
- Tutto compila e gira su telefono con la stessa interfaccia di Windows (anche se ancora scomoda).

### Fase 4: touch, telefono e TV
- Tocco: tocco = clic, scorrimento verticale delle liste con inerzia, tocco sul video per mostrare
  e nascondere i comandi, doppio tocco per i salti di 10 secondi nei film, scorrimento laterale tra
  le sezioni.
- Dimensioni: densità dello schermo (dp), margini dei bordi con i tagli (notch), vertical e
  orizzontale, tastiera su schermo che non copre i campi.
- Tasto Indietro di Android collegato alla stessa logica di «Indietro» del telecomando.
- Android TV: manifest con `LEANBACK_LAUNCHER`, banner, `Nav.tv` attivo da solo, frecce e OK dal
  telecomando; «Apprendimento guidato» per i telecomandi che mandano tasti strani.

### Fase 5: pacchetto e distribuzione
- APK (`cargo-ndk` + Gradle, oppure `cargo-apk2`) con icona, nome, versione presa da `Cargo.toml`.
- Firma con la chiave dell'utente; in CI la chiave arriva da un segreto di GitHub Actions.
- Workflow GitHub Actions per Android: build, test, APK allegato alla stessa Release `vX.Y.Z`
  accanto agli installer di Windows, con file `.sha256`.
- Aggiornamento: l'avviso «nuova versione» (`update.rs`) apre la pagina della Release.
- Picture-in-Picture per il video; notifica di riproduzione con pausa (facoltativo).
- Se si sceglie F-Droid o Play: requisiti e metadati (descrizione, schermate, informativa).

### Fase 6: prove e rifinitura
- Almeno due dispositivi (uno vecchio e uno recente) e un emulatore.
- Liste grandi (50 000 canali) e guida da 500 000 programmi: tempi, memoria, batteria.
- Cambio di rete, schermo spento, chiamata in arrivo, rotazione, app in secondo piano per ore.
- Controllo delle licenze (`THIRD-PARTY.md`) con le librerie Android incluse.
- Release 0.6.0 (o 1.0.0, a tua scelta) con Windows e Android.

## Ordine e stima
Fase 0 (1-2 giorni, decide tutto) → 1 → 2 → 3 (giorni) → 4 → 5 → 6. Le fasi 1 e 2 non hanno
file in comune e si possono fare in sessioni separate dopo la fase 0.
