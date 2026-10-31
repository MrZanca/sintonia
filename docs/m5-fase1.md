# M5, fase 1: il core gira su Android

Esito: **`sintonia-core` compila e funziona su Android.** Ramo `m5-fase-1` (partito da `m5-fase-0`).
Tutti i test del core passano sul telefono; le parti che vogliono la JVM (Keystore, cartelle
dell'app, proprietà di sistema) sono state provate dentro un APK vero.

## Cosa è cambiato nel core

| Parte | Su Android |
|---|---|
| Collegamento con Java | Nuovo modulo `android.rs`: l'app chiama `sintonia_core::android::init(vm, activity)` una volta all'avvio (puntatori di `AndroidApp`), poi il core usa la JVM da qualunque thread. Senza `init` le funzioni rispondono «non inizializzato», niente panico. |
| Cartelle | `android::app_dirs()`: `Context.getFilesDir()` (uguale a `internal_data_path` di `android-activity`, controllato) e `Context.getCacheDir()`. Il resto del core riceve già i percorsi da fuori (`ServiceConfig`) e non cambia. `directories` era una dipendenza inutilizzata del core: tolta (la usa solo l'app). |
| Segreti | `keyring` solo fuori da Android. Su Android `SystemKeyring` (stessa interfaccia, stesso `database_key`) usa `vault.rs`: una chiave casuale da 32 byte cifra con `crypto.rs` (XChaCha20-Poly1305) l'elenco dei segreti in `files/segreti/<servizio>.segreti`; quella chiave è a sua volta cifrata (AES-256-GCM) da una chiave dell'**Android Keystore** che non esce mai dal Keystore, in `files/segreti/<servizio>.chiave`. Una sola operazione nel Keystore per avvio. Se la chiave del Keystore sparisce, i segreti si ripartono vuoti (l'archivio si ricrea, come con un portachiavi nuovo su Windows). |
| Rete | `service::http_client()` (ora pubblico, usato dal servizio): su Android legge il proxy del sistema dalle proprietà Java `http(s).proxyHost/Port` e `http.nonProxyHosts` (reqwest guarda solo le variabili d'ambiente, che le app Android non hanno) e usa come autorità di certificazione quelle di sistema lette dai file (`/apex/com.android.conscrypt/cacerts`, se no `/system/etc/security/cacerts`). DNS: quello di sistema con `getaddrinfo`, nessun cambiamento. |
| SQLite | Niente da cambiare: `libsqlite3-sys` compila SQLite per Android con `SQLITE_TEMP_STORE=3` (file temporanei in memoria), perché nelle app non c'è una cartella temporanea che SQLite conosca. Un test nuovo (`sorts_larger_than_the_cache_work`) lo verifica. |
| `PRAGMA integrity_check` | Rivisto: funziona uguale (il test della riparazione passa sul telefono senza `TMPDIR`). Le copie di riserva finiscono accanto all'archivio, nella cartella dati dell'app. |
| Istanza unica | Rivista, non toccata (sta nell'app): fuori da Windows l'apertura di `sintonia.lock` non blocca mai, quindi su Android non fa niente, ed è giusto, perché Android tiene un solo processo per app. In fase 3 il file di blocco va in `android::app_dirs().0`, come il resto. |

## Certificati: la scelta

Il piano diceva `rustls-platform-verifier` (il gestore dei certificati di Android). L'ho collegato e
provato, poi su decisione dell'utente l'ho tolto:

- vuole un componente Kotlin nell'APK (`org.rustls:rustls-platform-verifier` 0.2.0, AAR fuori da
  crates.io) e la libreria standard di Kotlin, cioè Gradle o un passaggio R8 a mano;
- controlla le revoche scaricando le liste (CRL) in HTTP in chiaro: senza l'apposita
  `network_security_config` (che con Gradle arriva dall'AAR) scambia per **revocati** certificati
  buoni (google.com, tutti quelli di Let's Encrypt, che non ha più OCSP);
- con il traffico in chiaro permesso, sul Wi-Fi di prova (IPv6 assegnato ma non funzionante) **ogni
  nuova connessione HTTPS impiegava circa 31 s**, contro 0,6 s senza.

Con le autorità di sistema dai file: niente Java, stesso metodo di mpv (fase 2,
`sintonia_player::tls`). Si perdono il controllo delle revoche e le autorità aggiunte dall'utente (che
le app Android dalla 7 non usano comunque, salvo configurazione apposita).

## Prove fatte (Xiaomi Mi 9T, Android 11, arm64-v8a, Wi-Fi)

- **Test del core sul telefono:** 83 su 83 passano (i 79 di Windows più 4 del modulo Android),
  lanciati senza `TMPDIR` e dalla cartella `/`, come dentro un'app. Tempo: circa 12 s.
- **Build:** `aarch64-linux-android` e `x86_64-linux-android` compilano; clippy con `-D warnings` pulito
  per Android. aws-lc-rs compila con la toolchain del NDK: ring non serve. Nessun emulatore: i test
  x86_64 non sono stati eseguiti.
- **APK di prova** (`prototipi/android-fase1`, `scripts/android-core-prova.ps1`, 6 MB):
  - `init` 0–2 ms; cartelle `/data/user/0/it.sintonia.provacore/{files,cache}`;
  - chiave dell'archivio: creazione nel Keystore 164 ms al primo avvio, poi 18–41 ms; la stessa chiave
    (impronta uguale) dopo il riavvio e dopo la reinstallazione con `adb install -r`;
  - segreto scritto, riletto, cancellato; nei file nessun testo in chiaro (né la password né i nomi);
  - HTTPS: github.com e google.com rispondono 200; scaduto, autofirmato, radice sconosciuta e nome
    sbagliato (badssl.com) rifiutati con l'errore giusto; `revoked.badssl.com` accettato (atteso,
    vedi sopra); HTTP in chiaro funziona.
- **Windows:** `cargo fmt --all --check`, `cargo clippy --all-targets --all-features` e `cargo test`
  puliti.

## Non provato

- Il proxy di sistema su un dispositivo: provata solo la conversione delle proprietà Java (test sul
  telefono). Impostarlo voleva cambiare un'impostazione del telefono (`settings put global
  http_proxy`), che non ho fatto. Un proxy automatico (PAC) non è gestito, e il proxy si legge solo
  quando si crea il client (cioè all'avvio del servizio).
- La chiave del Keystore che sparisce davvero (provato solo con una chiave finta nei test).
- Emulatore x86_64, Android TV, dispositivi a 32 bit, Android 14 e successivi (cartella Conscrypt).
- Tempi delle connessioni: sul Wi-Fi di prova i primi collegamenti variano da 0,6 a 8 s per il DNS e
  l'IPv6 della rete; non dipende dal core, ma conviene rimisurare in fase 6 su un'altra rete.

## Cosa resta per la fase 3

- Nell'app: chiamare `android::init` in `android_main`, prendere le cartelle da
  `android::app_dirs()` al posto di `directories` (che su Android non trova niente) e mettere lì
  `sintonia.lock`.
- Il profilo di prova (`SINTONIA_PROFILO`) su Android non ha senso (non ci sono variabili d'ambiente):
  decidere se toglierlo o leggerlo da un'impostazione.
- Il manifest non ha bisogno di `usesCleartextTraffic`: le connessioni di Rust (e di mpv) non passano
  dalle regole di rete di Java. Serve solo `android.permission.INTERNET`.
