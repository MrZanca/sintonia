//! Solo per M0: flussi pubblici di prova al posto dei canali finti della bozza.
//!
//! Sono stream di test messi a disposizione da chi li pubblica proprio per provare i player
//! (nessun canale televisivo). Spariscono in M1, quando i canali arrivano dalla sorgente
//! dell'utente.
//!
//! Con la variabile d'ambiente `SINTONIA_STREAM` (o il primo argomento da riga di comando) si
//! può provare un proprio indirizzo: vale per tutti i canali. Non viene salvato da nessuna parte.

/// Flussi HLS di prova: un live e alcuni on demand, con formati diversi (TS e fMP4).
const TEST_STREAMS: &[&str] = &[
    // Unified Streaming, flusso live di prova
    "https://demo.unified-streaming.com/k8s/live/stable/live.isml/.m3u8",
    // Mux, Big Buck Bunny (HLS con segmenti TS)
    "https://test-streams.mux.dev/x36xhzz/x36xhzz.m3u8",
    // Apple, esempio avanzato con segmenti fMP4
    "https://devstreaming-cdn.apple.com/videos/streaming/examples/img_bipbop_adv_example_fmp4/master.m3u8",
    // Unified Streaming, Tears of Steel
    "https://demo.unified-streaming.com/k8s/features/stable/video/tears-of-steel/tears-of-steel.ism/.m3u8",
];

/// L'indirizzo da aprire per il canale `index` della bozza.
pub fn stream_for(index: usize, overridden: Option<&str>) -> String {
    match overridden {
        Some(url) => url.to_owned(),
        None => TEST_STREAMS[index % TEST_STREAMS.len()].to_owned(),
    }
}

/// Indirizzo indicato dall'utente per la prova, se c'è.
pub fn override_from_env() -> Option<String> {
    std::env::args()
        .nth(1)
        .or_else(|| std::env::var("SINTONIA_STREAM").ok())
        .filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_index_has_a_stream() {
        for i in 0..20 {
            assert!(stream_for(i, None).starts_with("https://"));
        }
        assert_eq!(stream_for(3, Some("file.ts")), "file.ts");
    }
}
