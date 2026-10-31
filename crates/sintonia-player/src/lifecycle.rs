//! Quando sospendere e riprendere il video su Android.
//!
//! Con l'app in secondo piano o lo schermo spento la superficie della finestra sparisce e Slint
//! smette di disegnare, ma mpv continua a decodificare e scarta i fotogrammi (circa 40 al secondo
//! nella fase 0 di M5). L'app quindi si sospende quando smette di essere visibile *oppure* perde la
//! superficie, e riprende quando ha di nuovo entrambe. I due segnali arrivano in ordine diverso a
//! seconda del caso (tasto Home, schermo spento, blocco con l'impronta), per questo si tengono
//! separati. Qui c'è solo la decisione, senza Android, così si prova anche su Windows.

/// Un segnale del ciclo di vita dell'attività Android.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleEvent {
    /// `onStart`: l'attività è visibile.
    Started,
    /// `onStop`: non è più visibile (Home, altra app, schermo spento).
    Stopped,
    /// La superficie di disegno è pronta (`InitWindow`).
    WindowCreated,
    /// La superficie è stata distrutta (`TerminateWindow`).
    WindowDestroyed,
}

/// Cosa deve fare l'app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Mettere in pausa un film, fermare la diretta.
    Suspend,
    /// Riprendere quello che era stato sospeso.
    Resume,
}

/// Stato del ciclo di vita, aggiornato a ogni [`LifecycleEvent`].
#[derive(Debug, Default)]
pub struct Lifecycle {
    visible: bool,
    window: bool,
    /// Visibile e con la superficie almeno una volta: la prima volta non c'è niente da riprendere.
    started: bool,
}

impl Lifecycle {
    /// Visibile e con la superficie: il video si può mostrare.
    pub fn is_active(&self) -> bool {
        self.visible && self.window
    }

    /// Registra un segnale e dice se l'app deve sospendersi o riprendere.
    pub fn handle(&mut self, event: LifecycleEvent) -> Option<Transition> {
        let was_active = self.is_active();
        match event {
            LifecycleEvent::Started => self.visible = true,
            LifecycleEvent::Stopped => self.visible = false,
            LifecycleEvent::WindowCreated => self.window = true,
            LifecycleEvent::WindowDestroyed => self.window = false,
        }
        match (was_active, self.is_active()) {
            (true, false) => Some(Transition::Suspend),
            (false, true) if self.started => Some(Transition::Resume),
            (false, true) => {
                self.started = true;
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LifecycleEvent::{Started, Stopped, WindowCreated, WindowDestroyed};
    use super::*;

    fn run(events: &[LifecycleEvent]) -> Vec<Option<Transition>> {
        let mut lifecycle = Lifecycle::default();
        events.iter().map(|e| lifecycle.handle(*e)).collect()
    }

    #[test]
    fn first_start_resumes_nothing() {
        assert_eq!(run(&[Started, WindowCreated]), [None, None]);
        assert_eq!(run(&[WindowCreated, Started]), [None, None]);
    }

    #[test]
    fn home_and_back() {
        // Home: prima onStop, poi la superficie; al ritorno l'ordine opposto
        let got = run(&[
            Started,
            WindowCreated,
            Stopped,
            WindowDestroyed,
            Started,
            WindowCreated,
        ]);
        assert_eq!(
            got,
            [
                None,
                None,
                Some(Transition::Suspend),
                None,
                None,
                Some(Transition::Resume),
            ]
        );
    }

    #[test]
    fn surface_lost_first_suspends_once() {
        let got = run(&[
            Started,
            WindowCreated,
            WindowDestroyed,
            Stopped,
            WindowCreated,
            Started,
        ]);
        assert_eq!(
            got,
            [
                None,
                None,
                Some(Transition::Suspend),
                None,
                None,
                Some(Transition::Resume),
            ]
        );
    }

    #[test]
    fn locked_screen_resumes_only_with_surface() {
        // schermo spento con il blocco: l'attività torna visibile dietro la schermata di blocco
        // ma la superficie arriva solo dopo lo sblocco
        let mut lifecycle = Lifecycle::default();
        for e in [Started, WindowCreated, Stopped, WindowDestroyed, Started] {
            lifecycle.handle(e);
        }
        assert!(!lifecycle.is_active());
        assert_eq!(lifecycle.handle(WindowCreated), Some(Transition::Resume));
    }

    #[test]
    fn repeated_signals_are_ignored() {
        let got = run(&[Started, WindowCreated, Started, Stopped, Stopped]);
        assert_eq!(got, [None, None, None, Some(Transition::Suspend), None]);
    }
}
