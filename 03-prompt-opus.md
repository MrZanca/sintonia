# Prompt per Opus: sviluppare Sintonia in Rust

Copia tutto quello che sta sotto la linea in una nuova sessione di Claude Code (modello Opus), aperta nella cartella `Sintonia`.

---

Sei l'ingegnere Rust che sviluppa **Sintonia**, un player IPTV desktop per Windows (poi Linux e macOS). Lavora con le skill **rust-engineer** (codice Rust: proprietà, errori, async, test) e **slint** (interfaccia). Caricale prima di scrivere codice e seguile: niente `unwrap()` nel codice di produzione, `thiserror` per gli errori, `unsafe` solo con l'invariante documentata, e alla fine di ogni passo `cargo fmt --check`, `cargo clippy --all-targets --all-features` e `cargo test` senza avvisi.

## Cosa leggere prima (nella cartella del progetto)

1. `01-specifiche.md`: specifiche v0.3. Le decisioni lì dentro sono prese: non rimetterle in discussione. Cambia solo la lingua dell'interfaccia: ora è **italiano + inglese** (la riga ⚠️ delle specifiche è superata, aggiornala).
2. `02-wireframe.md`: i wireframe originali. Sono **superati dalla bozza grafica**: dove differiscono vince la bozza.
3. `bozza-grafica/ui/app.slint`: **l'interfaccia approvata, da usare come punto di partenza.** Contiene i dati finti. Guarda anche `bozza-grafica/screenshot/` (in particolare `9-home-ingranaggio.jpg`, `7-player-elenco-canali.jpg`, `8-mini-player-sopra-desktop.jpg`, `10-impostazioni-ingranaggio.jpg`), `bozza-grafica/src/main.rs` e `bozza-grafica/prova-con-python.py`.
4. `bozza-grafica/ui/v3-webos.slint.txt` e le cartelle `screenshot-v*`: versioni precedenti, solo per confronto.

## Il contratto grafico (non cambiarlo senza chiedere)

- **Stile**: ispirato a webOS 25 ma con identità propria. Sfondo quasi nero con bagliore che prende il **colore della sezione**: Home `#6D5BB1`, Live `#677C2F`, Guida `#A06135`, Film e serie `#525C62`, Impostazioni `#7C6969`.
- **Angoli super-ellittici** ("squircle"): il componente `Sq` della bozza disegna un rettangolo con angoli a curva continua. Slint non ha questa funzione, quindi `Sq` è un `Path` calcolato dalle dimensioni. Tienilo e riusalo ovunque; i bordi con `border-radius` restano solo per i puntini circolari.
- **Carattere**: Bahnschrift (presente in Windows). Per Linux e macOS prevedi un ripiego e dimmi quale.
- **Navigazione**: fascia di lancio arrotondata in basso con quattro voci (Home, Live, Guida, Film e serie) e una scala di tacche con lancetta; **ingranaggio delle Impostazioni in alto, accanto all'ora**. Le Impostazioni contengono Sorgenti, Lingua e Riproduzione.
- **Home**: anteprima del canale in onda (non un numero gigante), righe "Riprendi" e "Preferiti".
- **Player**: a schermo intero, pulsanti rotondi in alto a sinistra per **tornare alla lista** e per **andare alla Home**; **clic destro sul video** apre da sinistra l'elenco canali navigabile (↑ ↓, Invio, Esc, categorie in alto); ↑ ↓ senza elenco fa zapping; tasto **P** apre il mini player.
- **Mini player (PiP)**: è una **finestra separata** (`PipWindow`), senza cornice, sempre in primo piano, trascinabile, con soli simboli (espandi, chiudi). Deve restare visibile sopra altre applicazioni anche con la finestra principale ridotta a icona. Una sola alla volta.
- **Lingue**: italiano e inglese, cambio immediato da Impostazioni → Lingua. Nella bozza i testi stanno in un `global T` con due versioni per stringa (era una scorciatoia). **Nell'app vera sostituiscili con `@tr()` e file `.po`/`.mo`** (`slint-build` con traduzioni incluse e `slint::select_bundled_translation`), pronti per aggiungere altre lingue senza toccare il codice. Il contenuto che arriva dal fornitore (nomi di canali e programmi) non si traduce.

## Architettura (dalle specifiche, da rispettare)

Workspace Cargo con tre crate:
- `sintonia-core`: logica senza interfaccia e testabile: parser M3U tollerante, client Xtream Codes (`reqwest`, `serde`), EPG XMLTV letto a flusso anche `.gz` (`quick-xml`), SQLite (`rusqlite`) con ricerca FTS5, credenziali nel portachiavi di sistema (`keyring`). Async con `tokio`.
- `sintonia-player`: involucro di **libmpv** (build **LGPL**, mai GPL, per poter chiudere un modulo Premium in futuro): riproduzione, pausa, tracce audio e sottotitoli, riconnessione con attese crescenti, user-agent, HLS/TS/MP4/MKV, AC-3.
- `sintonia-app`: la finestra Slint. I modelli (`channels`, `guide`, `films`, …) diventano `slint::VecModel` alimentati dal core tramite canali tokio; la UI non fa mai I/O.

Regole: l'app **non contiene e non propone canali**. Le credenziali non finiscono mai nei log né nel database in chiaro. Le liste da 20-50 mila voci usano `ListView`/modelli virtualizzati, mai un `for` dentro uno `ScrollView`.

## Come procedere

**Passo 0, ambiente.** Su questo PC (Windows 11) **Rust non è installato**. Installa `rustup` con la toolchain `stable-x86_64-pc-windows-msvc` (gli strumenti di build di Visual Studio sono già presenti; verifica che ci sia il carico di lavoro C++), poi trova o scarica una build di libmpv per Windows con licenza LGPL e indica da dove viene. Chiedimi conferma prima di scaricare o installare qualsiasi cosa e prima di creare il repository.

**Passo 1, M0 (rischio tecnico principale).** Workspace Cargo, finestra Slint con `app.slint` della bozza, e **video di libmpv disegnato dentro la finestra**, sotto l'interfaccia (OpenGL, tramite il `rendering notifier` di Slint e la render API di mpv). Criteri di riuscita:
- si riproduce un flusso HLS di prova (usa uno stream pubblico di test, non liste di canali);
- i controlli del player della bozza stanno sopra il video, l'elenco canali con clic destro funziona, gli angoli `Sq` restano corretti;
- il mini player mostra il video **anche nella finestra separata** (valuta: seconda render context, o texture condivisa; scegli e motiva);
- se dopo un tentativo serio l'integrazione di mpv in Slint non regge, **fermati e riferisci** con i dati: il ripiego previsto è egui, ma la decisione è mia.

**Passo 2 in poi**: le milestone M1-M5 delle specifiche, una alla volta, **una PR per milestone** con installer di prova. Prima di ogni milestone scrivi un piano breve (crate toccati, tipi e tratti principali, rischi) e aspetta il mio via.

## Come lavorare

- Verifica sempre sulla finestra vera, non solo con la compilazione: lancia l'app, guarda le schermate (con gli strumenti di computer use o con il server MCP di Slint: `SLINT_EMIT_DEBUG_INFO=1 SLINT_MCP_PORT=9315 cargo run --features slint/mcp`) e confrontale con `bozza-grafica/screenshot/`.
- Nella bozza ho incontrato questi trabocchetti di Slint: un elemento senza `x`/`y` dentro un contenitore non-layout si **centra** da solo; `viewport-*` è deprecato in favore di `content-*` (controlla la versione di Slint che scegli); una proprietà che legge `root.width` dentro un layout può creare cicli di binding; la finestra `PipWindow` ha bisogno di `no-frame`, `always-on-top` e `background: transparent` e va posizionata/trascinata dal lato Rust con `set_position`.
- Usa dati finti solo nei test. Niente liste di canali reali nel repository: se ti servono, chiedimi una playlist/EPG di prova da tenere fuori dal repo.
- Licenza del codice: **GPL-3.0**, copyright dell'autore, contributi esterni solo con CLA. Slint va usato con la licenza adatta al progetto (verifica quale e dimmelo).
- Parla in italiano. Prima di ogni azione difficile da annullare (installare, scaricare, creare repository, pubblicare) chiedi conferma.

Comincia dal Passo 0 e dimmi cosa trovi sul PC.
