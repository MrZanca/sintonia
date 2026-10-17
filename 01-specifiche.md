# Sintonia: specifiche v0.3 (player IPTV per PC, Rust)

Stato: **pianificazione completa, niente codice finché non dai il via**. Tutte le decisioni sono prese.

## 1. Cosa è (e cosa non è)

Un **player** IPTV desktop: l'utente inserisce la *sua* sorgente (link M3U, file M3U o credenziali Xtream Codes) e l'app la riproduce con guida TV, preferiti, film e serie.

L'app **non contiene e non fornisce canali**. Niente liste precaricate, niente "fornitori consigliati", niente loghi di canali nel materiale promozionale.

## 2. Perché niente versione web

Un browser non riesce a riprodurre quasi nessuno stream IPTV da solo: i server non permettono l'accesso da altre pagine (CORS), quasi tutti sono in `http://` (bloccato nelle pagine `https://`) e molti canali sono MPEG-TS, che il browser non legge. L'unica scappatoia è un server che fa da ponte, con costi, banda e responsabilità legale su di te (ritrasmetti tu i flussi). Scegliendo Rust, la strada naturale è un'**app desktop nativa**, che non ha nessuno di questi limiti. La versione web è fuori.

## 3. Decisioni

| Tema | Scelta | Note |
|---|---|---|
| Piattaforma | **PC: Windows, poi Linux e macOS** (deciso) | niente web |
| Linguaggio | **Rust** (deciso) | |
| Sorgenti | **M3U/M3U8 (URL e file) + Xtream Codes + EPG XMLTV** (deciso) | Stalker/MAC al massimo in v2 |
| VOD | **Film e serie nella prima versione** (deciso) | |
| Distribuzione | **Gratuita su GitHub** (deciso): codice pubblico, installer nelle Release | |
| Licenza | **GPL-3.0** (deciso), copyright tutto tuo, contributi esterni solo con CLA | serve a poter monetizzare dopo |
| Nome | **Sintonia** (deciso), da verificare sul registro marchi EUIPO | niente "IPTV" nel nome |
| Interfaccia grafica | **Slint** (deciso) | ripiego su egui se il video di mpv non si integra in M0 |
| Lingua UI | **Italiano + inglese** (deciso) | cambio immediato da Impostazioni → Lingua; traduzioni con `@tr()` e file `.po`, pronte per altre lingue |

## 4. Funzioni

### MVP (v1)
1. **Gestione sorgenti**: URL M3U, file M3U locale, login Xtream Codes (server, utente, password, con verifica account). Più sorgenti salvate, una attiva.
2. **Parsing M3U tollerante**: `tvg-id`, `tvg-name`, `tvg-logo`, `group-title`, `#EXTGRP`, URL EPG nell'intestazione (`url-tvg`/`x-tvg-url`), voci duplicate, file sporchi.
3. **Canali live**: categorie, ricerca istantanea, loghi, numerazione.
4. **Player** (libmpv): HLS, MPEG-TS, MP4, MKV, audio AC-3; riconnessione automatica con attese crescenti; tracce audio e sottotitoli; schermo intero; zapping con frecce ↑↓.
5. **EPG XMLTV**: download anche `.gz`, lettura a flusso (i file arrivano a centinaia di MB), abbinamento per `tvg-id` con fallback sul nome, "ora in onda / dopo" nella lista e nel player.
6. **Preferiti** e **ultimi visti** per sorgente.
7. **VOD e serie** (Xtream): elenco, stagioni ed episodi, riprendi da dove eri.
8. **Impostazioni**: user-agent personalizzato, intervallo di aggiornamento playlist/EPG, PIN su categorie.

### v2
- Griglia guida EPG completa e catch-up/timeshift se il provider lo supporta.
- Registrazione su disco (solo desktop).
- Multi-vista 2×2, Picture-in-Picture.
- Scorciatoie da tastiera complete e supporto telecomando/gamepad.
- Backup/ripristino impostazioni.

### Fuori scopo
- Liste di canali incluse o consigliate.
- Versione web, e qualsiasi server nostro che riceva credenziali o ritrasmetta stream.

## 5. Architettura (Rust)

```
┌───────────────────────────── sintonia-app (interfaccia, Slint) ─────────────────────────┐
│  Home · Live · Guida · Film · Serie · Player · Sorgenti · Impostazioni                   │
│  il video di mpv è disegnato sotto l'interfaccia (OpenGL), i controlli sopra             │
└──────────────▲──────────────────────────────────────────────────────▲───────────────────┘
               │ comandi / eventi (canali tokio)                      │
┌──────────────┴──────────── sintonia-core ───────────────┐  ┌────────┴── sintonia-player ─┐
│ m3u      parser tollerante                              │  │ wrapper di libmpv           │
│ xtream   client player_api.php (reqwest, serde)         │  │ play/pausa/seek, tracce,    │
│ epg      XMLTV a flusso (quick-xml + gzip)              │  │ riconnessione, user-agent   │
│ db       SQLite (rusqlite) + ricerca full-text FTS5     │  │ HLS, TS, MP4, MKV, AC-3,    │
│ secrets  credenziali nel portachiavi di sistema (keyring)│  │ HEVC: legge tutto           │
└─────────────────────────────────────────────────────────┘  └────────────────────────────┘
```

Workspace Cargo con tre crate: `sintonia-core` (logica pura, testabile senza interfaccia), `sintonia-player` (video), `sintonia-app` (finestra). Async con tokio.

**Perché libmpv come motore video**: è il motore di mpv, legge praticamente qualsiasi formato e codec (incluso l'audio Dolby AC-3 dei canali sport, che i browser non leggono), gestisce lo streaming di rete da solo e regge bene i flussi che cadono. Scrivere un player da zero in Rust sarebbe follia; questo è lo standard.

**Perché Slint**: ha un linguaggio dichiarativo per le schermate, adatto a un'interfaccia "da TV" con focus e tastiera, e supporta in modo documentato il disegno OpenGL sotto l'interfaccia, che è esattamente quello che serve per il video di mpv. egui è più semplice da integrare ma ha un aspetto da strumento per sviluppatori.

### Punti che fanno male, detti subito
- **Rust è più lento da sviluppare** di uno stack web: compila lento, l'ecosistema di interfacce grafiche è giovane. In cambio: app da ~20-30 MB, poca RAM, avvio istantaneo, niente problemi di codec.
- **libmpv va distribuito con l'app**: su Windows si include la DLL nell'installer, su Linux si dichiara come dipendenza, su macOS va impacchettato nel bundle (è la piattaforma più rognosa, per questo arriva per ultima).
- **Licenze e monetizzazione futura**: libmpv compilato in modalità GPL obbliga tutta l'app a restare GPL, modulo Premium compreso. Va usata una build **LGPL** di libmpv (si può, rinunciando a pochi filtri opzionali). Slint per il modulo chiuso richiede la sua licenza gratuita "royalty-free" con attribuzione. Tutto fattibile, ma va deciso prima, non dopo.
- **EPG enormi**: lettura a flusso e scrittura a blocchi su SQLite, mai tutto in memoria.
- **Playlist da 20-50k voci**: liste virtualizzate e ricerca FTS5.
- **Abbinamento EPG/canale**: `tvg-id` spesso mancanti o sbagliati, serve fallback per nome e correzione manuale.
- **Firma**: senza certificato, Windows mostra "PC protetto" e macOS blocca l'app finché non la autorizzi. Normale per i progetti gratuiti.
- **Build automatiche**: GitHub Actions compila Windows/Linux/macOS a ogni versione e pubblica nelle Release, gratis per i repository pubblici.

## 6. Monetizzazione (dopo il periodo gratuito)

- **Da subito**: donazioni (GitHub Sponsors, Ko-fi). Incassi poco, ma misuri l'interesse.
- **Dopo**: base gratuita e open source, funzioni **Premium** a pagamento (multi-vista, registrazione, sincronizzazione tra PC, temi) in un modulo chiuso con chiave di licenza.
- **No**: pubblicità (pochi soldi, utenti infastiditi) e abbonamenti obbligatori sulla base già gratuita.
- **Rischi**: alcuni processori di pagamento guardano male tutto ciò che è "IPTV" (da verificare con Stripe/Paddle prima di partire); vendere con continuità richiede partita IVA.

## 7. Piano di lavoro (a milestone)

1. **M0** Workspace Cargo, finestra Slint, video libmpv dentro la finestra. È il rischio tecnico più grosso, quindi si fa per primo.
2. **M1** Sorgente M3U + lista canali + player live. Prima versione già usabile.
3. **M2** Xtream Codes (live, film, serie) + preferiti + ultimi visti + riprendi.
4. **M3** EPG: ora/dopo, poi griglia guida.
5. **M4** Impostazioni, PIN, installer Windows, build automatiche su GitHub.
6. **M5** Build Linux e macOS.

Ogni milestone = una PR con installer di prova.

## 8. Cosa serve prima di scrivere codice
- Il tuo via libera a partire con il codice (M0).
- Un **account GitHub** e un **repository pubblico** collegato al progetto (guida nel thread).
- Opzionale ma prezioso: una tua playlist/EPG reale per i test (non finisce nel repository).
