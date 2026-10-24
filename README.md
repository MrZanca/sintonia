<p align="center"><img src="assets/logo/sintonia-logo-256.png" alt="Logo di Sintonia" width="128"></p>

# Sintonia

Player IPTV per PC (Windows, poi Linux e macOS), scritto in Rust con interfaccia Slint e video libmpv.

Sintonia è **solo un player**: non contiene e non propone canali. Si usa con la propria playlist
M3U o il proprio account Xtream Codes.

Stato: **M4**, versione completa per Windows.
- Playlist M3U da link o da file e account Xtream Codes con film e serie; più sorgenti salvate, una in uso.
- Canali per categoria, ricerca istantanea, loghi, numerazione, preferiti, «Riprendi».
- Guida dei programmi XMLTV: «in onda / dopo» e griglia della Guida.
- Player con riconnessione automatica, tracce audio e sottotitoli, muto, mini player, schermo intero.
- Aggiornamento automatico di playlist e guida, PIN sulle categorie (controllo genitori).
- Interfaccia in italiano e inglese, cambio immediato.
- Installer per Windows e versione portatile nelle Release, compilati da GitHub Actions.

In arrivo: Linux e macOS (M5). Vedi `01-specifiche.md`.

- Sviluppo e compilazione: [docs/sviluppo.md](docs/sviluppo.md)
- Licenza: [GPL-3.0](LICENSE), copyright © 2026 MrZanca; librerie di terze parti in [THIRD-PARTY.md](THIRD-PARTY.md)
- Contributi: [CONTRIBUTING.md](CONTRIBUTING.md)
