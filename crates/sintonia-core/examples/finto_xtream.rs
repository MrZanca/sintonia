//! Avvia il finto server Xtream per provare l'app senza un account vero.
//!
//! ```text
//! cargo run -p sintonia-core --features mock --example finto_xtream -- [porta] [canali film serie]
//! ```
//!
//! Poi in Sintonia: Impostazioni › Sorgenti › Xtream Codes, con l'indirizzo stampato qui sotto,
//! utente `mario`, password `prova123`. Il catalogo è inventato; i flussi sono stream pubblici di
//! prova. Si ferma con Ctrl+C.

use sintonia_core::xtream_mock::{Catalog, MockServer, PASSWORD, USERNAME};

fn main() {
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8899);
    let sizes: Vec<usize> = args.filter_map(|a| a.parse().ok()).collect();
    let catalog = match sizes.as_slice() {
        [live, movies, series] => Catalog::large(*live, *movies, *series),
        _ => Catalog::sample(),
    };
    let server = MockServer::start_on(catalog, port);
    println!("Finto server Xtream su {}", server.address());
    println!("utente: {USERNAME}   password: {PASSWORD}");
    loop {
        std::thread::park();
    }
}
