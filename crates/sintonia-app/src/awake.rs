//! Impedisce lo spegnimento dello schermo e la sospensione mentre si guarda un canale.
//!
//! Su Windows si usa `SetThreadExecutionState`, che vale finché il thread che la chiama (quello
//! dell'interfaccia) è vivo, e si annulla da sola alla chiusura dell'app. mpv lo farebbe da sé
//! con la sua finestra, ma qui disegna dentro Slint (`vo=libmpv`) e non lo fa.
//! Linux e macOS arrivano con M5.

use std::cell::Cell;

#[cfg(windows)]
mod imp {
    const ES_CONTINUOUS: u32 = 0x8000_0000;
    const ES_SYSTEM_REQUIRED: u32 = 0x0000_0001;
    const ES_DISPLAY_REQUIRED: u32 = 0x0000_0002;

    #[link(name = "kernel32")]
    extern "system" {
        fn SetThreadExecutionState(flags: u32) -> u32;
    }

    pub fn set(awake: bool) {
        let flags = if awake {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: la funzione riceve solo un intero e modifica lo stato di alimentazione del
        // thread chiamante; non ci sono puntatori né invarianti da rispettare.
        let previous = unsafe { SetThreadExecutionState(flags) };
        if previous == 0 {
            log::warn!("impossibile cambiare lo stato di alimentazione dello schermo");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn set(_awake: bool) {}
}

/// Tiene acceso lo schermo finché `set(true)`; da usare solo dal thread dell'interfaccia.
#[derive(Debug, Default)]
pub struct KeepAwake {
    active: Cell<bool>,
}

impl KeepAwake {
    /// Accende o spegne la richiesta (chiama il sistema solo quando lo stato cambia).
    pub fn set(&self, awake: bool) {
        if self.active.replace(awake) != awake {
            log::debug!("schermo sempre acceso: {awake}");
            imp::set(awake);
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.set(false);
    }
}
