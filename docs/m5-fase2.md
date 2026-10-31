# M5, fase 2: il video su Android

Esito: **il video regge su Android con una libmpv LGPL compilata da noi.** Su Mi 9T (Android 11,
arm64-v8a): H.264 e HEVC 10 bit a 1080p a 50–60 fps con `mediacodec`, nessun fotogramma perso a
regime. Sospensione e ripresa funzionano con il tasto Home e con lo schermo spento e il blocco a
impronta. La CPU resta alta (circa 60% di un core) e non sono riuscito a ridurla.

## Cosa è stato fatto

1. **libmpv LGPL per Android** (`scripts/libmpv-android.sh`, workflow `libmpv Android`). Usa le
   `buildscripts` di mpv-android a commit fissato e cambia solo due righe: FFmpeg senza
   `--enable-gpl`, mpv con `-Dgpl=false`. Resta `--enable-version3`, perché mbedtls è Apache-2.0:
   FFmpeg quindi è LGPLv3, come la build per Windows. mpv è allo stesso commit di Windows
   (`6f605372b9`, API client 2.6). Le dipendenze sono fissate a una versione o a un tag.
   Alla fine lo script controlla quattro cose e si ferma se una non torna: il `config.h` di FFmpeg
   («LGPL version 3 or later»), la riga di configurazione (niente `gpl`, `nonfree`, x264, x265),
   `HAVE_GPL 0` in mpv, e che ogni `.so` dipenda solo da librerie del pacchetto o di sistema.
   Ho controllato anche a mano la stringa di licenza dentro i binari: «LGPL version 3 or later» in
   tutte le librerie di FFmpeg, per entrambe le ABI.
   La compilazione su GitHub dura 13 minuti (esecuzione `41205587384`). Versione e SHA-256 sono in
   `docs/sviluppo.md`, licenze e dipendenze in `THIRD-PARTY.md`.
2. **`sintonia-player/build.rs`**: c'è il ramo Android. Prende `SINTONIA_MPV_DIR` o
   `vendor/mpv-android`, con una cartella per ABI. Collega `libmpv` e `libavcodec` (per
   `av_jni_set_java_vm`) e passa la cartella come `DEP_MPV_LIB_DIR` a chi deve impacchettare le `.so`.
3. **Stato OpenGL**: `video.rs` ora salva e ripristina attorno a mpv tutto lo stato che usa FemtoVG,
   su tutti i sistemi: framebuffer, viewport, texture delle unità 0 e 1, programma, VAO (se c'è),
   buffer, blend, scissor, stencil, depth, cull, maschere e colore di cancellazione. Su Windows costa
   una trentina di letture di stato per fotogramma video.
   Le **dimensioni dell'immagine** erano già a posto: in `VideoView` l'`Image` ha `width: 100%` e
   `height: 100%`, quindi non cresce da sola come nel prototipo.
4. **Opzioni**:
   - `hwdec=mediacodec,mediacodec-copy`; se nessuno dei due accetta il flusso, mpv decodifica in
     software da sé.
   - `ao=audiotrack`.
   - `sintonia_player::set_java_vm`.
5. **Certificati HTTPS** (non previsto nel piano). La nuova mpv apre gli `https` con libcurl e
   verifica i certificati, ma su Android libcurl e FFmpeg (mbedtls) non trovano quelli di sistema:
   ogni flusso `https` falliva con «TLS certificate verification failed».
   `sintonia_player::tls::system_ca_bundle` raccoglie le autorità di sistema (Conscrypt da Android 14,
   altrimenti `/system/etc/security/cacerts`; 138 sul Mi 9T) in un PEM per `tls-ca-file`.
   Controllo: un certificato scaduto (`expired.badssl.com`) viene rifiutato.
   Su Windows non cambia niente: lì libcurl usa i certificati di sistema.
6. **Ciclo di vita**: `sintonia_player::lifecycle::Lifecycle` decide quando sospendere e quando
   riprendere. L'app si sospende quando non è più visibile (`onStop`) *o* perde la superficie, e
   riprende quando ha di nuovo tutte e due; i casi sono provati con test che girano anche su Windows.
   `VideoRouter::suspend`/`resume` fanno quello che serve:
   - un film va in pausa, e riparte solo se non l'aveva già messo in pausa l'utente;
   - la diretta si ferma, e al ritorno si riapre il canale.

   Il `RenderContext` si rilascia da solo: Slint chiama il *teardown* del rendering quando arriva la
   nuova superficie, e `video.rs` libera il contesto di mpv e lo ricrea.
   L'app su Android non esiste ancora (fase 3), quindi `suspend`/`resume` per ora non le chiama
   nessuno. Il banco di prova (il prototipo della fase 0, aggiornato) applica la stessa regola.

## Prove sul dispositivo

Xiaomi Mi 9T (Snapdragon 730, Adreno 618, Android 11, arm64-v8a), banco di prova
`prototipi/android-fase0` con la libmpv LGPL. Le prove del ciclo di vita le ho fatte prima anche con la
build GPL della fase 0, con lo stesso esito.

| Prova | Esito |
|---|---|
| Avvio (`am start -W`) | 335–432 ms; contesto video dopo 170–230 ms |
| H.264 1080p60, HLS (`test-streams.mux.dev`) | `mediacodec`, 60/60 fps, 0 persi a regime, avsync ≈ 0 |
| HEVC Main 10 1080p60, HLS fMP4 (esempio pubblico di Apple) | `mediacodec`, 60/60 fps, 0 persi; primo fotogramma in 2 s |
| Diretta HLS con segmenti MPEG-TS (canale italiano dalle liste dell'utente), 1080p50 | `mediacodec`, 50/50 fps, 0 persi |
| Tasto Home e ritorno (diretta) | sospesa alla perdita della superficie, 0 fotogrammi in secondo piano; ripresa in 3 s a 60 fps |
| Schermo spento, sblocco con impronta (diretta) | sospesa e ripresa a 60 fps, 0 persi |
| Tasto Home e ritorno (film) | pausa e ripresa dallo stesso punto |
| Certificato scaduto | rifiutato |

**CPU** (media di 10–20 s, % di un core su 8; misurata a regime, perché nei primi 60–90 s si riempie
la cache e scaricare con TLS costa da solo fino al 40%):

| Caso | CPU |
|---|---|
| H.264 1080p60, `mediacodec` (predefinito) | 59–63% |
| come sopra con `profile=fast` | 64% (nessun guadagno) |
| `mediacodec-copy` | 69% |
| variante HLS minima 320×184 a 30 fps | 38% |
| solo audio (`vid=no`) | circa 9% |
| HEVC Main 10 1080p60 | 65% |

Per thread: il thread dell'interfaccia (FemtoVG che ridisegna tutta la finestra più il disegno di mpv)
prende circa il 32%, un thread senza nome (`Thread-2`, che sale e scende con i fotogrammi disegnati) il 13%, MediaCodec il 13%. Il costo dipende
dai fotogrammi da ridisegnare, non dai filtri di mpv. Abbassarlo vuol dire ridisegnare meno
l'interfaccia (solo il video, o niente sotto il video a schermo intero): è un lavoro per la fase 6. In
questa fase non è stato ridotto.

**Riproduzione lunga**: 33 minuti di diretta HLS con segmenti MPEG-TS (1080p50, `mediacodec`), con un
campione al minuto. Risultati:
- nessuna interruzione e nessuna riconnessione;
- 50 fps fissi;
- 86 fotogrammi persi dall'uscita video su circa 99 000 (0,09%), in tre gruppi: al 17°, al 20° e al
  31° minuto; il decoder non ne ha persi;
- avsync sempre sotto 0,1 ms;
- CPU 53–60%;
- memoria (PSS) stabile tra 253 e 263 MB, senza crescita;
- temperatura della batteria da 28,6 a 31,9 °C, con il telefono collegato via USB e lo schermo acceso.

## Rischi e cose emerse

- **Il redattore degli indirizzi ha un buco.** Un indirizzo con un BOM davanti (causato dal mio script
  di prova, poi corretto) arriva a mpv come percorso. mpv scrive allora «Failed to open
  /(BOM)https:/host/…», e `sintonia_core::redact` non lo riconosce, quindi l'indirizzo finisce per
  intero nel log. Nelle prove erano indirizzi pubblici senza credenziali. Va corretto in
  `sintonia-core` (fase 1 o dopo): riconoscere anche `https:/` con una sola barra e i caratteri
  invisibili davanti.
- **Log rumoroso**: con alcuni canali FFmpeg scrive «mpegts: Invalid timestamps» decine di volte al
  secondo (circa 1200 righe in 20 s a livello `info`). Con il livello `warn` dell'app vanno controllati.
- **Script Lua integrati**: mpv avvia i suoi script interni (`select`, `positioning`, `commands`,
  `context_menu`) anche con `load-scripts=no`. Restano fermi ma occupano thread e memoria. Si possono
  spegnere con le opzioni `load-*=no`, anche su Windows (non fatto: cambia anche Windows).
- La libreria è più grande di quella della fase 0: l'APK di prova passa da 36 a 42,6 MB, con
  `libavcodec` a 12 MB e `libmpv` a 8,5 MB dopo `strip`.
- Gli artefatti di GitHub scadono dopo 90 giorni: la build va rilanciata o conservata (fase 5:
  allegarla alla Release o a un'altra Release interna).
- **Non provati**:
  - flussi `.ts` continui come quelli dei fornitori Xtream (nelle liste dell'utente non ce ne sono);
  - AV1;
  - HDR su schermo;
  - emulatore x86_64 (la libreria c'è);
  - dispositivi a 32 bit (non c'è una build armeabi-v7a);
  - chiamata in arrivo e cambio di rete;
  - riproduzione per ore (provati 33 minuti);
  - il ciclo di vita dentro l'app vera (fase 3).
