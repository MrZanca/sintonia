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

use crate::{MainWindow, Nav};

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
        _ => None,
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
        let WinitEvent::KeyboardInput { event, .. } = event else {
            return EventResult::Propagate;
        };
        let Some(main) = weak.upgrade() else {
            return EventResult::Propagate;
        };
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
        // le frecce e le lettere le gestisce già Slint
        assert_eq!(remote_action(&WinitKey::Named(NamedKey::ArrowUp)), None);
        assert_eq!(remote_action(&WinitKey::Character("a".into())), None);
    }

    #[test]
    fn navigation_keys() {
        assert!(is_navigation(&WinitKey::Named(NamedKey::ArrowLeft)));
        assert!(is_navigation(&WinitKey::Named(NamedKey::Escape)));
        assert!(!is_navigation(&WinitKey::Named(NamedKey::Tab)));
        assert!(!is_navigation(&WinitKey::Character("s".into())));
    }
}
