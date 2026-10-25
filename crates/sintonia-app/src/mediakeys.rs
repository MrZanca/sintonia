//! Tasti del volume (e muto) dei telecomandi e delle tastiere multimediali.
//!
//! Questi tasti Windows li prende per sé e cambia il volume generale, prima che arrivino alla
//! finestra. Con Sintonia in primo piano (e l'impostazione accesa) li si intercetta prima di
//! Windows con un aggancio alla tastiera (`WH_KEYBOARD_LL`) e regolano invece il volume del player;
//! con un'altra finestra in primo piano passano a Windows come sempre.
//! Alcuni dispositivi mandano invece un `WM_APPCOMMAND` alla finestra: anche quello si ferma qui.

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

/// Chi riceve i tasti: il tasto e se è la ripetizione di un tasto tenuto premuto.
type Handler = Box<dyn Fn(VolumeKey, bool)>;

thread_local! {
    /// Chi riceve i tasti (sul thread dell'interfaccia, lo stesso della finestra e dell'aggancio).
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

/// Passa il tasto a Sintonia; `false` se nessuno lo riceve.
fn deliver(key: VolumeKey, repeat: bool) -> bool {
    HANDLER.with(|h| match h.try_borrow() {
        Ok(handler) => handler.as_ref().map(|f| f(key, repeat)).is_some(),
        Err(_) => false,
    })
}

/// Un tasto del volume arrivato alla finestra come tasto normale (quando l'aggancio non l'ha
/// fermato prima): lo si passa comunque a Sintonia. `true` se è stato preso.
pub fn from_window(key: VolumeKey, repeat: bool) -> bool {
    ENABLED.load(Ordering::Relaxed) && deliver(key, repeat)
}

#[cfg(windows)]
mod imp {
    use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

    use super::{command, deliver, volume_key, volume_vk, ENABLED};

    const GWLP_WNDPROC: i32 = -4;
    const WH_KEYBOARD_LL: i32 = 13;
    const WM_APPCOMMAND: u32 = 0x0319;
    const WM_KEYDOWN: usize = 0x0100;
    const WM_SYSKEYDOWN: usize = 0x0104;

    /// `KBDLLHOOKSTRUCT`
    #[repr(C)]
    struct KeyboardHook {
        vk_code: u32,
        scan_code: u32,
        flags: u32,
        time: u32,
        extra_info: usize,
    }

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
        fn SetWindowsHookExW(id: i32, proc_: isize, module: isize, thread: u32) -> isize;
        fn CallNextHookEx(hook: isize, code: i32, wparam: usize, lparam: isize) -> isize;
        fn GetForegroundWindow() -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, process: *mut u32) -> u32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> isize;
    }

    /// La finestra principale e la sua procedura originale.
    static WINDOW: AtomicIsize = AtomicIsize::new(0);
    static PREVIOUS: AtomicIsize = AtomicIsize::new(0);
    /// Un tasto del volume è già giù: le pressioni successive sono ripetizioni.
    static HELD: [AtomicBool; 3] = [
        AtomicBool::new(false),
        AtomicBool::new(false),
        AtomicBool::new(false),
    ];

    /// La finestra appartiene a questo processo.
    fn is_ours(hwnd: isize) -> bool {
        if hwnd == 0 {
            return false;
        }
        let mut pid = 0u32;
        // SAFETY: `pid` è una variabile locale valida in cui Windows scrive l'id del processo.
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        pid == std::process::id()
    }

    extern "system" fn keyboard_hook(code: i32, wparam: usize, lparam: isize) -> isize {
        // code < 0: va solo passato avanti; lParam punta a un KBDLLHOOKSTRUCT valido
        if code >= 0 && lparam != 0 && ENABLED.load(Ordering::Relaxed) {
            // SAFETY: per WH_KEYBOARD_LL con code >= 0 Windows passa in lParam un puntatore a un
            // KBDLLHOOKSTRUCT valido per la durata della chiamata; lo si legge soltanto.
            let info = unsafe { &*(lparam as *const KeyboardHook) };
            let ours = WINDOW.load(Ordering::Relaxed);
            // SAFETY: nessun argomento; restituisce solo un handle.
            let foreground = unsafe { GetForegroundWindow() };
            if let Some(key) = volume_vk(info.vk_code) {
                log::debug!(
                    "aggancio: {key:?} messaggio {wparam:#x}, finestra in primo piano {foreground:#x}, la nostra {ours:#x}"
                );
                // in primo piano c'è una finestra di Sintonia (l'handle in primo piano non è
                // sempre quello che winit dà per la finestra principale: si guarda il processo)
                if ours != 0 && is_ours(foreground) {
                    let held = &HELD[(info.vk_code - 0xAD) as usize];
                    if wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN {
                        let repeat = held.swap(true, Ordering::Relaxed);
                        log::debug!("tasto del volume {key:?} (ripetuto: {repeat})");
                        if deliver(key, repeat) {
                            return 1;
                        }
                    } else {
                        held.store(false, Ordering::Relaxed);
                        // anche il rilascio non deve arrivare a Windows
                        return 1;
                    }
                }
            }
        }
        // SAFETY: si passa l'evento al prossimo aggancio con gli stessi parametri ricevuti.
        unsafe { CallNextHookEx(0, code, wparam, lparam) }
    }

    extern "system" fn window_proc(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> isize {
        if msg == WM_APPCOMMAND && ENABLED.load(Ordering::Relaxed) {
            if let Some(key) = volume_key(command(lparam)) {
                log::debug!("WM_APPCOMMAND del volume {key:?}");
                if deliver(key, false) {
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
        WINDOW.store(hwnd, Ordering::Relaxed);
        // SAFETY: GetModuleHandleW(NULL) restituisce il modulo dell'eseguibile; `keyboard_hook` ha
        // la firma di una HOOKPROC e l'aggancio vive quanto l'app (il thread dell'interfaccia ha
        // il ciclo dei messaggi che Windows richiede).
        let hook = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD_LL,
                keyboard_hook as *const () as usize as isize,
                GetModuleHandleW(std::ptr::null()),
                0,
            )
        };
        if hook == 0 {
            log::warn!("aggancio alla tastiera non riuscito");
        } else {
            log::debug!("aggancio alla tastiera attivo");
        }
        let ours = window_proc as *const () as usize as isize;
        // SAFETY: `hwnd` è la finestra principale, viva per tutta l'app e creata da questo thread;
        // `window_proc` ha la firma di una WNDPROC e richiama sempre la procedura precedente.
        let previous = unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, ours) };
        if previous != 0 {
            PREVIOUS.store(previous, Ordering::Relaxed);
        }
        hook != 0 || previous != 0
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
}
