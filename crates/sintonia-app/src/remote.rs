//! Telecomando.
//!
//! - Le frecce che nessun elemento usa spostano il fuoco al seguente o al precedente: l'interfaccia
//!   chiama `Nav.step` e qui si manda un Tab (o Maiusc+Tab) finto, che Slint sa già gestire.
//! - I tasti speciali dei telecomandi (Indietro, Home, Play/Pausa) arrivano da winit con nomi che
//!   Slint non traduce: diventano tasti che l'interfaccia conosce.
//! - Se il fuoco si perde (l'elemento che lo aveva è sparito) un tasto non arriva a nessuno: lo si
//!   rimette in un posto sicuro, così il telecomando non resta mai senza effetto.

use std::time::Duration;

use slint::platform::{Key, WindowEvent};
use slint::winit_030::winit::event::WindowEvent as WinitEvent;
use slint::winit_030::winit::keyboard::{Key as WinitKey, NamedKey};
use slint::winit_030::{EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, SharedString};

use crate::mediakeys::VolumeKey;
use crate::{Keys, MainWindow, Nav};

/// Cosa fare per un tasto speciale del telecomando.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Lo si sostituisce con questo tasto.
    Press(Key),
    /// Torna alla Home (chiudendo il player).
    Home,
}

/// I tasti dei telecomandi che Slint non conosce.
pub fn remote_action(key: &WinitKey) -> Option<Action> {
    let WinitKey::Named(named) = key else {
        return None;
    };
    match named {
        NamedKey::BrowserBack | NamedKey::GoBack => Some(Action::Press(Key::Escape)),
        NamedKey::BrowserHome | NamedKey::GoHome => Some(Action::Home),
        NamedKey::MediaPlayPause | NamedKey::MediaPlay | NamedKey::MediaPause => {
            Some(Action::Press(Key::Space))
        }
        NamedKey::Select => Some(Action::Press(Key::Return)),
        // i tasti in più dei telecomandi diventano F13…F24, che si possono assegnare in
        // Impostazioni › Tasti
        NamedKey::BrowserFavorites => Some(Action::Press(Key::F13)),
        NamedKey::BrowserSearch => Some(Action::Press(Key::F14)),
        NamedKey::BrowserRefresh => Some(Action::Press(Key::F15)),
        NamedKey::BrowserForward => Some(Action::Press(Key::F16)),
        NamedKey::MediaTrackNext => Some(Action::Press(Key::F17)),
        NamedKey::MediaTrackPrevious => Some(Action::Press(Key::F18)),
        NamedKey::MediaStop => Some(Action::Press(Key::F19)),
        NamedKey::LaunchMail => Some(Action::Press(Key::F20)),
        NamedKey::LaunchApplication1 => Some(Action::Press(Key::F21)),
        NamedKey::LaunchMediaPlayer => Some(Action::Press(Key::F22)),
        NamedKey::Info => Some(Action::Press(Key::F23)),
        NamedKey::Guide => Some(Action::Press(Key::F24)),
        NamedKey::ChannelUp => Some(Action::Press(Key::PageUp)),
        NamedKey::ChannelDown => Some(Action::Press(Key::PageDown)),
        _ => None,
    }
}

/// Come si chiama un tasto (Impostazioni › Navigazione, «Ultimo tasto premuto»): la lettera, il
/// nome del tasto speciale, o il codice se Windows non sa che tasto sia.
pub fn key_name(
    logical: &WinitKey,
    physical: &slint::winit_030::winit::keyboard::PhysicalKey,
) -> String {
    match logical {
        WinitKey::Character(c) if c.trim().is_empty() => "Space".to_owned(),
        WinitKey::Character(c) => c.to_uppercase(),
        WinitKey::Named(named) => format!("{named:?}"),
        _ => format!("{physical:?}"),
    }
}

/// I tasti con cui ci si muove: se non arrivano a nessun elemento il fuoco si era perso.
fn is_navigation(key: &WinitKey) -> bool {
    matches!(
        key,
        WinitKey::Named(
            NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Enter
                | NamedKey::Escape
        )
    )
}

/// Pressione e rilascio di un tasto, come se venissero dalla tastiera.
fn press(main: &MainWindow, key: Key) {
    let text = SharedString::from(key);
    main.window()
        .dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    main.window()
        .dispatch_event(WindowEvent::KeyReleased { text });
}

/// Preme `key` appena finita la gestione dell'evento in corso.
pub fn press_later(main: &MainWindow, key: Key) {
    later(main, move |main| press(main, key));
}

/// Esegue `f` appena finita la gestione dell'evento in corso (dentro, Slint è ancora occupato).
fn later(main: &MainWindow, f: impl FnOnce(&MainWindow) + 'static) {
    let weak = main.as_weak();
    slint::Timer::single_shot(Duration::ZERO, move || {
        if let Some(main) = weak.upgrade() {
            f(&main);
        }
    });
}

pub fn install(main: &MainWindow) {
    let weak = main.as_weak();
    main.global::<Nav>().on_step(move |forward| {
        if let Some(main) = weak.upgrade() {
            later(&main, move |main| {
                press(main, if forward { Key::Tab } else { Key::Backtab });
            });
        }
    });

    let weak = main.as_weak();
    main.window().on_winit_window_event(move |_, event| {
        // i tasti del volume si prendono solo mentre la finestra è attiva (vedi mediakeys.rs)
        if let WinitEvent::Focused(focused) = event {
            crate::mediakeys::set_focused(*focused);
        }
        let WinitEvent::KeyboardInput { event, .. } = event else {
            return EventResult::Propagate;
        };
        let Some(main) = weak.upgrade() else {
            return EventResult::Propagate;
        };
        log::debug!(
            "tasto {:?} ({:?}) {}",
            event.logical_key,
            event.physical_key,
            if event.state.is_pressed() {
                "premuto"
            } else {
                "lasciato"
            }
        );
        if event.state.is_pressed() && !event.repeat {
            main.global::<Keys>()
                .set_last(key_name(&event.logical_key, &event.physical_key).into());
        }
        // i tasti del volume, se arrivano fin qui, regolano il player (vedi mediakeys.rs)
        let volume = match &event.logical_key {
            WinitKey::Named(NamedKey::AudioVolumeUp) => Some(VolumeKey::Up),
            WinitKey::Named(NamedKey::AudioVolumeDown) => Some(VolumeKey::Down),
            WinitKey::Named(NamedKey::AudioVolumeMute) => Some(VolumeKey::Mute),
            _ => None,
        };
        if let Some(key) = volume {
            if event.state.is_pressed() && crate::mediakeys::from_window(key, event.repeat) {
                return EventResult::PreventDefault;
            }
        }
        if let Some(action) = remote_action(&event.logical_key) {
            if event.state.is_pressed() {
                log::debug!("telecomando: {:?} → {action:?}", event.logical_key);
                later(&main, move |main| match action {
                    Action::Press(key) => press(main, key),
                    Action::Home => main.invoke_go_home(),
                });
            }
            return EventResult::PreventDefault;
        }
        let nav = main.global::<Nav>();
        if nav.get_tv() && event.state.is_pressed() && is_navigation(&event.logical_key) {
            // la radice dell'interfaccia lo rimette a true se il tasto arriva a un elemento
            nav.set_key_seen(false);
            later(&main, |main| {
                if !main.global::<Nav>().get_key_seen() {
                    log::debug!("telecomando: fuoco perso, lo si rimette a posto");
                    main.invoke_recover_focus();
                }
            });
        }
        EventResult::Propagate
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_keys() {
        let back = WinitKey::Named(NamedKey::BrowserBack);
        assert_eq!(remote_action(&back), Some(Action::Press(Key::Escape)));
        let home = WinitKey::Named(NamedKey::BrowserHome);
        assert_eq!(remote_action(&home), Some(Action::Home));
        let play = WinitKey::Named(NamedKey::MediaPlayPause);
        assert_eq!(remote_action(&play), Some(Action::Press(Key::Space)));
        let fav = WinitKey::Named(NamedKey::BrowserFavorites);
        assert_eq!(remote_action(&fav), Some(Action::Press(Key::F13)));
        // le frecce e le lettere le gestisce già Slint
        assert_eq!(remote_action(&WinitKey::Named(NamedKey::ArrowUp)), None);
        assert_eq!(remote_action(&WinitKey::Character("a".into())), None);
    }

    #[test]
    fn key_names() {
        use slint::winit_030::winit::keyboard::{KeyCode, PhysicalKey};
        let physical = PhysicalKey::Code(KeyCode::KeyA);
        assert_eq!(key_name(&WinitKey::Character("a".into()), &physical), "A");
        assert_eq!(
            key_name(&WinitKey::Named(NamedKey::AudioVolumeUp), &physical),
            "AudioVolumeUp"
        );
    }

    #[test]
    fn navigation_keys() {
        assert!(is_navigation(&WinitKey::Named(NamedKey::ArrowLeft)));
        assert!(is_navigation(&WinitKey::Named(NamedKey::Escape)));
        assert!(!is_navigation(&WinitKey::Named(NamedKey::Tab)));
        assert!(!is_navigation(&WinitKey::Character("s".into())));
    }
}
