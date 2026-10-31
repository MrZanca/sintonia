# Prompt per lo sviluppo di M5 (Android) con Opus

Come si usa: incollare il blocco qui sotto in una nuova sessione di Claude Code (modello Opus) aperta
nella cartella del progetto, cambiando solo la riga «FASE DA FARE». Una fase per sessione.

```text
Lavori su Sintonia, un'app IPTV gratuita (GPL-3.0) scritta in Rust: sintonia-core (logica, SQLite,
M3U/Xtream, guida), sintonia-player (involucro di libmpv, render API OpenGL) e sintonia-app
(interfaccia Slint, renderer FemtoVG, backend winit). Oggi gira su Windows (versione 0.5.0, repository
pubblico MrZanca/sintonia). La milestone M5 porta Sintonia su Android.

FASE DA FARE: Fase 0   (cambia in 1, 2, 3, 4, 5 o 6 per le sessioni successive)

PRIMA DI TUTTO leggi, in quest'ordine: docs/m5-android.md (le fasi e i punti di decisione),
docs/sviluppo.md, CONTRIBUTING.md, THIRD-PARTY.md, crates/sintonia-player/build.rs e
crates/sintonia-app/src/video.rs. Se esistono, leggi anche i documenti delle fasi già fatte
(docs/m5-fase0.md, ...). Esegui SOLO la fase indicata, niente delle fasi seguenti.

COME LAVORI
- Rispondi e commenta in italiano, come il resto del progetto. I commenti spiegano il perché, non il
  cosa; stessa densità, stessi nomi e stesso stile del codice vicino.
- Il computer è Windows 11 con PowerShell. Il codice per Windows deve continuare a compilare e a
  passare `cargo fmt --all --check`, `cargo clippy --all-targets --all-features` e `cargo test` a
  ogni passo. Tutto il codice nuovo per Android sta dietro `cfg(target_os = "android")`.
- Regole del workspace: niente `unwrap()` (clippy lo nega), ogni blocco `unsafe` ha un commento
  `// SAFETY:` che spiega perché è corretto, niente URL con utente e password nei log (usa
  `sintonia_core::redact`). Le stringhe dell'interfaccia sono in inglese con `@tr()` e la traduzione
  italiana sta in crates/sintonia-app/lang/it/LC_MESSAGES/sintonia-app.po.
- Licenze: libmpv deve essere LGPL (niente GPL, niente x264/x265). Ogni libreria nuova va in
  THIRD-PARTY.md con licenza; versione e SHA-256 dei file scaricati vanno scritti in
  docs/sviluppo.md, e l'SHA-256 si controlla davvero prima di usarli.
- Segreti: la chiave di firma dell'APK, le password e i token non vanno mai nel repository, nei log
  o nei messaggi. Non chiederli all'utente: indica dove li deve mettere lui.
- Verifica sul serio: dove puoi, esegui la prova (build per `aarch64-linux-android`, test, avvio su
  emulatore o dispositivo con adb). Se una cosa non l'hai potuta provare, scrivilo chiaramente nel
  rapporto finale invece di dire che funziona.
- Non fare `git push`, non creare tag né Release e non toccare il repository remoto senza che
  l'utente lo chieda. Lavora su un ramo `m5-fase-N`, con commit piccoli e messaggi in italiano che
  finiscono con la riga di attribuzione richiesta dalla sessione.
- Se serve una decisione dell'utente (dispositivo di prova, distribuzione, scelta tra due
  strade) fermati e chiedila con una domanda sola e le opzioni; non scegliere per lui sulle
  decisioni elencate in docs/m5-android.md.
- Se trovi che il piano è sbagliato (una libreria non c'è, una strada non regge), non forzarla:
  scrivi cosa hai trovato, proponi l'alternativa e fermati al punto di decisione.

CONSEGNA DELLA FASE
1. Il codice e i test della fase, sul ramo m5-fase-N.
2. Aggiorna docs/sviluppo.md con i comandi nuovi (ambiente, build, installazione sul dispositivo).
3. Per la fase 0 scrivi docs/m5-fase0.md con: cosa hai provato, numeri misurati (avvio, fotogrammi,
   decodifica hardware), la scelta finale tra mpv-in-Slint e ExoPlayer, e i rischi rimasti. Per le
   altre fasi aggiorna lo stato in docs/m5-android.md.
4. Un rapporto breve per l'utente: cosa funziona e come l'hai verificato, cosa non hai potuto
   verificare, cosa deve fare lui (cavo USB, abilitare il debug USB, firmare, ...), e la fase
   successiva consigliata.
```

## Note per chi scrive i prompt successivi

- Fase 0 decide tutto: finché non c'è `docs/m5-fase0.md` non si lanciano le altre.
- Le fasi 1 e 2 non toccano gli stessi file e si possono fare in due sessioni separate, ognuna sul
  suo ramo; la fase 3 parte quando entrambe sono unite.
- Per la fase 4 (touch) conviene allegare alla sessione qualche schermata dell'app sul telefono.
