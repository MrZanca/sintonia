# Contribuire a Sintonia

Grazie dell'interesse. Prima di aprire una pull request:

- **CLA obbligatorio.** I contributi esterni si accettano solo dopo aver firmato un accordo di
  licenza per i contributori (CLA), che lascia all'autore il copyright necessario a gestire la
  licenza del progetto. Apri prima una issue: il testo del CLA ti viene indicato lì.
- Sintonia non contiene e non propone canali: niente playlist, URL di fornitori o loghi di canali
  nel repository, nemmeno nei test.
- Il codice deve passare `cargo fmt --check`, `cargo clippy --all-targets --all-features` e
  `cargo test` senza avvisi (vedi [docs/sviluppo.md](docs/sviluppo.md)).
