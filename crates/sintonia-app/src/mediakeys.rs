//! Tasti del volume (e muto) e altri tasti speciali dei telecomandi e delle tastiere multimediali.
//!
//! Windows li prende per sé (cambia il volume generale, apre la posta, il browser, comanda gli
//! altri lettori), anche se la finestra in primo piano è Sintonia. Mentre la finestra principale è
//! attiva Sintonia li registra come tasti di scelta rapida (`RegisterHotKey`): così arrivano solo a
//! Sintonia, come `WM_HOTKEY`. I tasti del volume regolano il player (se l'impostazione è accesa);
//! gli altri fanno quello che dice `remote.rs`. Quando la finestra perde il fuoco Sintonia li
//! restituisce e tornano a Windows.
//! Alcuni dispositivi mandano invece un `WM_APPCOMMAND` alla finestra: anche quello si prende qui.

use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Cosa chiede il tasto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKey {
    Up,
    Down,
    Mute,
}

/// I tasti del volume vanno a Sintonia (se no a Windows, come prima).
static ENABLED: AtomicBool = AtomicBool::new(true);

/// Accende o spegne la presa dei tasti del volume (Impostazioni › Navigazione).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    FOCUSED.with(|f| update(f.get()));
}

/// La finestra principale ha preso o perso il fuoco.
pub fn set_focused(focused: bool) {
    FOCUSED.with(|f| f.set(focused));
    update(focused);
}

/// Chi riceve i tasti: il tasto e se è la ripetizione di un tasto tenuto premuto.
type Handler = Box<dyn Fn(VolumeKey, bool)>;
/// Chi riceve gli altri tasti speciali (il tasto virtuale di Windows).
type OtherHandler = Box<dyn Fn(u32)>;

thread_local! {
    /// Chi riceve i tasti (sul thread dell'interfaccia, lo stesso della finestra).
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
    static OTHER: RefCell<Option<OtherHandler>> = const { RefCell::new(None) };
    static FOCUSED: Cell<bool> = const { Cell::new(false) };
    /// L'ultimo tasto arrivato e quando: per riconoscere le ripetizioni.
    static LAST: Cell<Option<(VolumeKey, Instant)>> = const { Cell::new(None) };
}

/// Il comando di un `WM_APPCOMMAND` (la parte alta di `lParam` senza i bit del dispositivo).
pub fn command(lparam: isize) -> u32 {
    ((lparam as usize >> 16) & 0x0fff) as u32
}

/// I comandi del volume (vedi `APPCOMMAND_VOLUME_*` in winuser.h).
pub fn volume_key(command: u32) -> Option<VolumeKey> {
    match command {
        8 => Some(VolumeKey::Mute),
        9 => Some(VolumeKey::Down),
        10 => Some(VolumeKey::Up),
        _ => None,
    }
}

/// I tasti virtuali del volume (`VK_VOLUME_MUTE`, `VK_VOLUME_DOWN`, `VK_VOLUME_UP`).
pub fn volume_vk(vk: u32) -> Option<VolumeKey> {
    match vk {
        0xAD => Some(VolumeKey::Mute),
        0xAE => Some(VolumeKey::Down),
        0xAF => Some(VolumeKey::Up),
        _ => None,
    }
}

/// Di quanto cambia il volume: a ogni pressione 5, tenendo premuto (il telecomando ripete il
/// tasto molte volte al secondo) 2 per volta.
pub fn step(key: VolumeKey, repeat: bool) -> i32 {
    let amount = if repeat { 2 } else { 5 };
    match key {
        VolumeKey::Up => amount,
        VolumeKey::Down => -amount,
        VolumeKey::Mute => 0,
    }
}

/// Un tasto è la ripetizione del precedente se è lo stesso e arriva entro poco.
pub fn is_repeat(previous: Option<(VolumeKey, Instant)>, key: VolumeKey, now: Instant) -> bool {
    previous.is_some_and(|(k, at)| k == key && now.duration_since(at) < Duration::from_millis(250))
}

/// Passa il tasto a Sintonia; `false` se nessuno lo riceve.
fn deliver(key: VolumeKey, repeat: bool) -> bool {
    HANDLER.with(|h| match h.try_borrow() {
        Ok(handler) => handler.as_ref().map(|f| f(key, repeat)).is_some(),
        Err(_) => false,
    })
}

/// Chi riceve gli altri tasti speciali (da chiamare sul thread dell'interfaccia).
pub fn set_other_handler(handler: impl Fn(u32) + 'static) {
    OTHER.with(|h| *h.borrow_mut() = Some(Box::new(handler)));
}

/// Gli altri tasti speciali che si prendono a Windows: avanti e indietro del browser, aggiorna,
/// cerca, preferiti, home, traccia seguente e precedente, stop, play/pausa, posta, lettore
/// multimediale, applicazione 1 (il tasto «e» di molti telecomandi).
pub const OTHER_KEYS: [u32; 13] = [
    0xA6, 0xA7, 0xA8, 0xAA, 0xAB, 0xAC, 0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6,
];

fn deliver_other(vk: u32) -> bool {
    OTHER.with(|h| match h.try_borrow() {
        Ok(handler) => handler.as_ref().map(|f| f(vk)).is_some(),
        Err(_) => false,
    })
}

/// Un tasto del volume arrivato alla finestra come tasto normale (quando non era registrato):
/// lo si passa comunque a Sintonia. `true` se è stato preso.
pub fn from_window(key: VolumeKey, repeat: bool) -> bool {
    ENABLED.load(Ordering::Relaxed) && deliver(key, repeat)
}

#[cfg(windows)]
fn update(focused: bool) {
    imp::register_volume(focused && ENABLED.load(Ordering::Relaxed));
    imp::register_other(focused);
}

#[cfg(not(windows))]
fn update(_focused: bool) {}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
    use std::time::Instant;

    use super::{
        command, deliver, deliver_other, is_repeat, volume_key, volume_vk, ENABLED, LAST,
        OTHER_KEYS,
    };

    const GWLP_WNDPROC: i32 = -4;
    const WM_APPCOMMAND: u32 = 0x0319;
    const WM_HOTKEY: u32 = 0x0312;
    /// I tasti del volume registrati: id (1, 2, 3) e tasto virtuale.
    const HOTKEYS: [(i32, u32); 3] = [(1, 0xAF), (2, 0xAE), (3, 0xAD)];
    /// Gli altri tasti hanno id da 100 in su (100 + posizione in `OTHER_KEYS`).
    const OTHER_BASE: i32 = 100;

    #[link(name = "user32")]
    extern "system" {
        fn SetWindowLongPtrW(hwnd: isize, index: i32, value: isize) -> isize;
        fn CallWindowProcW(
            prev: isize,
            hwnd: isize,
            msg: u32,
            wparam: usize,
            lparam: isize,
        ) -> isize;
        fn RegisterHotKey(hwnd: isize, id: i32, modifiers: u32, vk: u32) -> i32;
        fn UnregisterHotKey(hwnd: isize, id: i32) -> i32;
    }

    /// La finestra principale e la sua procedura originale.
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static PREVIOUS: AtomicIsize = AtomicIsize::new(0);
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    static OTHER_REGISTERED: AtomicBool = AtomicBool::new(false);

    /// Prende (`true`) o restituisce gli altri tasti speciali.
    pub fn register_other(on: bool) {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if hwnd == 0 || OTHER_REGISTERED.load(Ordering::Relaxed) == on {
            return;
        }
        for (i, vk) in (0..).zip(OTHER_KEYS) {
            let id = OTHER_BASE + i;
            if on {
                // SAFETY: come per i tasti del volume, con id che sono solo nostri.
                if unsafe { RegisterHotKey(hwnd, id, 0, vk) } == 0 {
                    log::debug!("tasto speciale {vk:#x} già preso da un altro programma");
                }
            } else {
                // SAFETY: si toglie la registrazione fatta sopra (se non c'era, non fa niente).
                unsafe { UnregisterHotKey(hwnd, id) };
            }
        }
        OTHER_REGISTERED.store(on, Ordering::Relaxed);
    }

    /// Prende (`true`) o restituisce i tasti del volume.
    pub fn register_volume(on: bool) {
        let hwnd = WINDOW.load(Ordering::Relaxed);
        if hwnd == 0 || REGISTERED.load(Ordering::Relaxed) == on {
            return;
        }
        for (id, vk) in HOTKEYS {
            if on {
                // SAFETY: `hwnd` è la finestra principale, viva per tutta l'app; si registra un
                // tasto senza modificatori con un id che è solo nostro.
                let ok = unsafe { RegisterHotKey(hwnd, id, 0, vk) };
                if ok == 0 {
                    log::warn!("tasto del volume {vk:#x} già preso da un altro programma");
                }
            } else {
                // SAFETY: si toglie la registrazione fatta sopra (se non c'era, non fa niente).
                unsafe { UnregisterHotKey(hwnd, id) };
            }
        }
        REGISTERED.store(on, Ordering::Relaxed);
        log::debug!(
            "tasti del volume {}",
            if on { "presi" } else { "restituiti" }
        );
    }

    fn take(key: super::VolumeKey) -> bool {
        let now = Instant::now();
        let repeat = LAST.with(|l| {
            let repeat = is_repeat(l.get(), key, now);
            l.set(Some((key, now)));
            repeat
        });
        log::debug!("tasto del volume {key:?} (ripetuto: {repeat})");
        deliver(key, repeat)
    }

    extern "system" fn window_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
        if msg == WM_HOTKEY {
            let vk = HOTKEYS
                .iter()
                .find(|(id, _)| usize::try_from(*id).is_ok_and(|id| id == wparam))
                .map(|(_, vk)| *vk);
            if let Some(key) = vk.and_then(volume_vk) {
                take(key);
                return 0;
            }
            let other =
                usize::try_from(i64::try_from(wparam).unwrap_or(-1) - i64::from(OTHER_BASE))
                    .ok()
                    .and_then(|i| OTHER_KEYS.get(i));
            if let Some(vk) = other {
                log::debug!("tasto speciale {vk:#x}");
                deliver_other(*vk);
                return 0;
            }
        }
        if msg == WM_APPCOMMAND && ENABLED.load(Ordering::Relaxed) {
            if let Some(key) = volume_key(command(lparam)) {
                if take(key) {
                    // TRUE: comando eseguito, la shell non cambia il volume di Windows
                    return 1;
                }
            }
        }
        // SAFETY: `PREVIOUS` è la procedura che la finestra aveva prima di `install`, e i
        // parametri sono quelli ricevuti da Windows, passati senza modifiche.
        unsafe { CallWindowProcW(PREVIOUS.load(Ordering::Relaxed), hwnd, msg, wparam, lparam) }
    }

    pub fn install(hwnd: isize) -> bool {
        if WINDOW.load(Ordering::Relaxed) != 0 {
            return true;
        }
        let ours = window_proc as *const () as usize as isize;
        // SAFETY: `hwnd` è la finestra principale, viva per tutta l'app e creata da questo thread;
        // `window_proc` ha la firma di una WNDPROC e richiama sempre la procedura precedente.
        let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, ours) };
        if previous == 0 {
            return false;
        }
        PREVIOUS.store(previous, Ordering::Relaxed);
        WINDOW.store(hwnd, Ordering::Relaxed);
        true
    }
}

/// Collega i tasti del volume della finestra principale a `handler`.
#[cfg(windows)]
pub fn install(window: &slint::Window, handler: impl Fn(VolumeKey, bool) + 'static) -> bool {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;

    HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(handler)));
    let hwnd = window.with_winit_window(|w| match w.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    });
    let installed = hwnd.flatten().is_some_and(imp::install);
    if installed {
        log::debug!("tasti del volume collegati");
        // la finestra potrebbe essere già attiva
        let focused = window.with_winit_window(|w| w.has_focus()).unwrap_or(false);
        set_focused(focused);
    } else {
        log::warn!("tasti del volume non collegati: restano a Windows");
    }
    installed
}

#[cfg(not(windows))]
pub fn install(_window: &slint::Window, _handler: impl Fn(VolumeKey, bool) + 'static) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_commands() {
        // lParam con APPCOMMAND_VOLUME_UP (10) e il bit del tasto (FAPPCOMMAND_KEY = 0)
        assert_eq!(command(10 << 16), 10);
        // con il bit del mouse (0x8000) il comando non cambia
        assert_eq!(command(((0x8000 | 9) << 16) as isize), 9);
        assert_eq!(volume_key(10), Some(VolumeKey::Up));
        assert_eq!(volume_key(9), Some(VolumeKey::Down));
        assert_eq!(volume_key(8), Some(VolumeKey::Mute));
        assert_eq!(volume_key(14), None);
    }

    #[test]
    fn volume_virtual_keys() {
        assert_eq!(volume_vk(0xAF), Some(VolumeKey::Up));
        assert_eq!(volume_vk(0xAE), Some(VolumeKey::Down));
        assert_eq!(volume_vk(0xAD), Some(VolumeKey::Mute));
        assert_eq!(volume_vk(0xB3), None);
        assert_eq!(step(VolumeKey::Up, false), 5);
        assert_eq!(step(VolumeKey::Down, true), -2);
        assert_eq!(step(VolumeKey::Mute, false), 0);
    }

    #[test]
    fn repeats() {
        let t = Instant::now();
        let later = t + Duration::from_millis(100);
        let much_later = t + Duration::from_millis(900);
        assert!(!is_repeat(None, VolumeKey::Up, t));
        assert!(is_repeat(Some((VolumeKey::Up, t)), VolumeKey::Up, later));
        assert!(!is_repeat(Some((VolumeKey::Up, t)), VolumeKey::Down, later));
        assert!(!is_repeat(
            Some((VolumeKey::Up, t)),
            VolumeKey::Up,
            much_later
        ));
    }
}
