//! Tasti del volume (e muto) dei telecomandi e delle tastiere multimediali.
//!
//! Windows li trasforma in un messaggio `WM_APPCOMMAND` che, se nessuno lo ferma, arriva alla shell
//! e cambia il volume generale. Con Sintonia in primo piano (e l'impostazione accesa) lo si ferma
//! qui e si cambia invece il volume del player. Per intercettarlo si mette una procedura davanti a
//! quella della finestra (winit non lo passa a Slint); tutto il resto va avanti come prima.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cosa chiede il tasto.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeKey {
    Up,
    Down,
    Mute,
}

/// I tasti del volume vanno a Sintonia (se no a Windows, come prima).
static ENABLED: AtomicBool = AtomicBool::new(true);

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// Chi riceve i tasti.
type Handler = Box<dyn Fn(VolumeKey)>;

thread_local! {
    /// Chi riceve i tasti (sul thread dell'interfaccia, lo stesso della finestra).
    static HANDLER: RefCell<Option<Handler>> = const { RefCell::new(None) };
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

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicIsize, Ordering};

    use super::{command, volume_key, ENABLED, HANDLER};

    const GWLP_WNDPROC: i32 = -4;
    const WM_APPCOMMAND: u32 = 0x0319;

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
    }

    /// La procedura originale della finestra.
    static PREVIOUS: AtomicIsize = AtomicIsize::new(0);

    extern "system" fn window_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
        if msg == WM_APPCOMMAND && ENABLED.load(Ordering::Relaxed) {
            if let Some(key) = volume_key(command(lparam)) {
                let handled = HANDLER.with(|h| match h.try_borrow() {
                    Ok(handler) => handler.as_ref().map(|f| f(key)).is_some(),
                    Err(_) => false,
                });
                if handled {
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
        if PREVIOUS.load(Ordering::Relaxed) != 0 {
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
        true
    }
}

/// Collega i tasti del volume della finestra principale a `handler`.
#[cfg(windows)]
pub fn install(window: &slint::Window, handler: impl Fn(VolumeKey) + 'static) -> bool {
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
    } else {
        log::warn!("tasti del volume non collegati: restano a Windows");
    }
    installed
}

#[cfg(not(windows))]
pub fn install(_window: &slint::Window, _handler: impl Fn(VolumeKey) + 'static) -> bool {
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
}
