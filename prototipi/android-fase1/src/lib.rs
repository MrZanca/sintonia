//! Prova di M5 fase 1: il core di Sintonia dentro un'app Android vera. Niente interfaccia: le
//! prove girano all'avvio e i risultati vanno nel logcat (etichetta `sintonia-prova`) e in
//! `prova-core.txt` nella cartella dei dati (`adb shell run-as it.sintonia.provacore cat
//! files/prova-core.txt`). Le parti che una semplice prova con adb non raggiunge: la JVM, il
//! Keystore, le cartelle dell'app e la verifica dei certificati di Android.

use std::fmt::Write as _;
use std::hash::{Hash, Hasher};
use std::time::Instant;

use android_activity::{AndroidApp, MainEvent, PollEvent};
use sintonia_core::secrets::{self, SecretStore, SystemKeyring};

const SERVICE: &str = "Sintonia-prova";

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("sintonia-prova"),
    );
    let mut report = String::new();
    run(&app, &mut report);
    for line in report.lines() {
        log::info!("{line}");
    }
    if let Some(dir) = app.internal_data_path() {
        if let Err(err) = std::fs::write(dir.join("prova-core.txt"), &report) {
            log::error!("prova-core.txt: {err}");
        }
    }
    log::info!("FINE");
    let mut quit = false;
    while !quit {
        app.poll_events(None, |event| {
            if let PollEvent::Main(MainEvent::Destroy) = event {
                quit = true;
            }
        });
    }
}

fn run(app: &AndroidApp, out: &mut String) {
    let t = Instant::now();
    // SAFETY: i puntatori vengono da `AndroidApp`, che tiene vive JavaVM e activity finché
    // `android_main` non ritorna.
    let init = unsafe { sintonia_core::android::init(app.vm_as_ptr(), app.activity_as_ptr()) };
    let _ = writeln!(out, "init: {init:?} in {} ms", t.elapsed().as_millis());
    if init.is_err() {
        return;
    }

    match sintonia_core::android::app_dirs() {
        Ok((data, cache)) => {
            let _ = writeln!(out, "dati: {}", data.display());
            let _ = writeln!(out, "cache: {}", cache.display());
            let same = app.internal_data_path().as_deref() == Some(data.as_path());
            let _ = writeln!(out, "uguale a internal_data_path: {same}");
        }
        Err(err) => {
            let _ = writeln!(out, "cartelle: ERRORE {err}");
        }
    }

    let t = Instant::now();
    match secrets::database_key(SERVICE) {
        Ok((key, created)) => {
            // un'impronta per confrontare i riavvii, non la chiave
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            key.hash(&mut hasher);
            let _ = writeln!(
                out,
                "chiave dell'archivio: creata={created}, impronta {:016x}, {} ms",
                hasher.finish(),
                t.elapsed().as_millis()
            );
        }
        Err(err) => {
            let _ = writeln!(out, "chiave dell'archivio: ERRORE {err}");
        }
    }
    let t = Instant::now();
    let again = secrets::database_key(SERVICE).map(|(_, created)| created);
    let _ = writeln!(
        out,
        "seconda lettura: creata={again:?}, {} ms",
        t.elapsed().as_millis()
    );

    let store = SystemKeyring {
        service: SERVICE.to_owned(),
    };
    let before = store.get("sorgente-1");
    let set = store.set("sorgente-1", "password-di-prova");
    let read = store.get("sorgente-1");
    let roundtrip = matches!(&read, Ok(Some(v)) if v == "password-di-prova");
    let _ = writeln!(
        out,
        "segreto: prima {}, scrittura {:?}, rilettura giusta {roundtrip}",
        if matches!(before, Ok(Some(_))) {
            "presente (avvio precedente)"
        } else {
            "assente"
        },
        set.map_err(|e| e.to_string())
    );
    let _ = store.set("sorgente-2", "da-cancellare");
    let deleted = store
        .delete("sorgente-2")
        .and_then(|()| store.get("sorgente-2"));
    let _ = writeln!(
        out,
        "cancellazione: {:?}",
        deleted.map_err(|e| e.to_string())
    );

    if let Ok((data, _)) = sintonia_core::android::app_dirs() {
        let dir = data.join("segreti");
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let bytes = std::fs::read(entry.path()).unwrap_or_default();
            let text = String::from_utf8_lossy(&bytes);
            let _ = writeln!(
                out,
                "file {}: {} byte, testo in chiaro: {}",
                entry.file_name().to_string_lossy(),
                bytes.len(),
                text.contains("password-di-prova") || text.contains("sorgente")
            );
        }
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = writeln!(out, "tokio: ERRORE {err}");
            return;
        }
    };
    let client = match sintonia_core::service::http_client("Sintonia-prova/0") {
        Ok(client) => client,
        Err(err) => {
            let _ = writeln!(out, "client HTTP: ERRORE {err}");
            return;
        }
    };
    for (url, expected) in [
        ("https://github.com/", "ok"),
        ("https://www.google.com/", "ok"),
        ("http://neverssl.com/", "ok"),
        ("https://expired.badssl.com/", "rifiuto"),
        ("https://self-signed.badssl.com/", "rifiuto"),
        ("https://untrusted-root.badssl.com/", "rifiuto"),
        ("https://wrong.host.badssl.com/", "rifiuto"),
        ("https://revoked.badssl.com/", "rifiuto (revoca)"),
    ] {
        let t = Instant::now();
        let result = runtime.block_on(async { client.get(url).send().await });
        let got = match result {
            Ok(response) => format!("HTTP {}", response.status().as_u16()),
            Err(err) => {
                let mut text = String::from("errore");
                let mut source: Option<&dyn std::error::Error> = Some(&err);
                while let Some(e) = source {
                    let _ = write!(text, " | {e}");
                    source = e.source();
                }
                text
            }
        };
        let _ = writeln!(
            out,
            "{url} (atteso {expected}): {got} in {} ms",
            t.elapsed().as_millis()
        );
    }
}
