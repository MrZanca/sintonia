//! Prove con la libmpv vera (senza finestra: nessun contesto di disegno).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use sintonia_player::{api_version, EndReason, Player, PlayerConfig, PlayerEvent};

fn player() -> (Player, mpsc::Receiver<PlayerEvent>) {
    let (tx, rx) = mpsc::channel();
    let player = Player::new(&PlayerConfig::default(), move |event| {
        let _ = tx.send(event);
    })
    .expect("mpv deve partire");
    (player, rx)
}

/// Aspetta il primo evento che soddisfa `wanted`, entro `timeout`.
fn wait_for(
    rx: &mpsc::Receiver<PlayerEvent>,
    timeout: Duration,
    wanted: impl Fn(&PlayerEvent) -> bool,
) -> Option<PlayerEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match rx.recv_timeout(left) {
            Ok(event) if wanted(&event) => return Some(event),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
}

#[test]
fn uses_client_api_2() {
    assert_eq!(api_version().0, 2);
}

#[test]
fn missing_file_reports_an_error() {
    let (player, rx) = player();
    player
        .load("C:/questo-file-non-esiste/sintonia.mkv")
        .expect("il comando loadfile viene accettato");
    let ended = wait_for(&rx, Duration::from_secs(10), |e| {
        matches!(e, PlayerEvent::Ended(_))
    });
    assert!(
        matches!(ended, Some(PlayerEvent::Ended(EndReason::Error(_)))),
        "atteso un errore, arrivato {ended:?}"
    );
}

#[test]
fn commands_on_an_idle_player_do_not_fail() {
    let (player, _rx) = player();
    player.set_volume(50).expect("volume");
    player.stop().expect("stop senza flusso");
}

#[test]
fn strings_with_nul_are_rejected() {
    let (player, _rx) = player();
    assert!(player.load("a\0b").is_err());
}

#[test]
fn per_file_options_are_accepted() {
    let (player, rx) = player();
    let options = sintonia_player::LoadOptions {
        user_agent: Some("Prova/1.0 (a, b)".into()),
        referrer: Some("https://r.example/".into()),
        start_secs: Some(30),
    };
    player
        .load_with("C:/questo-file-non-esiste/sintonia.ts", &options)
        .expect("loadfile con opzioni accettato");
    let ended = wait_for(&rx, Duration::from_secs(10), |e| {
        matches!(e, PlayerEvent::Ended(_))
    });
    assert!(matches!(
        ended,
        Some(PlayerEvent::Ended(EndReason::Error(_)))
    ));
    assert!(player.tracks().is_empty());
    assert_eq!(player.position(), None);
    assert_eq!(player.duration(), None);
    player.set_user_agent(None).expect("user-agent generale");
}
