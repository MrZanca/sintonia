//! Il video di mpv dentro le finestre Slint.
//!
//! # Come funziona
//!
//! Ogni finestra Slint (renderer FemtoVG) ha il suo contesto OpenGL. Nel *rendering notifier*,
//! prima che Slint disegni l'interfaccia, chiediamo a mpv di disegnare il fotogramma corrente in
//! una texture nostra, grande quanto il riquadro `VideoView` in pixel fisici. Poi un piccolo
//! shader rende trasparenti gli angoli con la stessa super-ellisse del componente `Sq`. La texture
//! diventa un'immagine Slint (`Video.frame`), quindi i controlli stanno sopra il video, gli angoli
//! restano esatti anche su sfondi non uniformi e il mini player può avere angoli trasparenti.
//!
//! # Mini player
//!
//! mpv ammette un solo contesto di disegno per core, legato a un solo contesto OpenGL, e Slint non
//! crea contesti condivisi tra finestre. Il contesto di disegno quindi **si sposta**: la finestra
//! che lo ha (`holder`) lo libera nel suo prossimo fotogramma, poi quella di destinazione
//! (`target`) lo ricrea nel suo. Il flusso di rete resta uno solo e la decodifica hardware resta
//! attiva; il costo è un attimo di nero durante il passaggio. In alternativa: due core di mpv
//! (doppia connessione, che molti fornitori IPTV non permettono) oppure copiare i fotogrammi
//! passando dalla CPU (non funziona con la finestra principale ridotta a icona, perché non disegna).

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CStr};
use std::num::NonZeroU32;
use std::rc::{Rc, Weak};
use std::time::Duration;

use glow::HasContext;
use sintonia_core::db::StreamInfo;
use sintonia_player::{Backoff, EndReason, LoadOptions, Player, PlayerEvent, RenderContext};
use slint::{ComponentHandle, GraphicsAPI, RenderingState};

use crate::awake::KeepAwake;
use crate::{MainWindow, PipWindow, Video};

/// Le finestre che possono mostrare il video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Main,
    Pip,
}

/// Valori di `Video.state` (vedi `app.slint`).
mod state {
    pub const IDLE: i32 = 0;
    pub const LOADING: i32 = 1;
    pub const PLAYING: i32 = 2;
    pub const BUFFERING: i32 = 3;
    pub const ERROR: i32 = 4;
    pub const RECONNECTING: i32 = 5;
}

/// Accesso uniforme alle due finestre.
trait VideoHost: ComponentHandle + 'static {
    const SURFACE: Surface;
    /// Il global `Video` di questa finestra.
    fn video(&self) -> Video<'_>;
    /// C'è un `VideoView` sullo schermo in questo momento.
    fn video_on_screen(&self) -> bool;
}

impl VideoHost for MainWindow {
    const SURFACE: Surface = Surface::Main;
    fn video(&self) -> Video<'_> {
        self.global::<Video>()
    }
    fn video_on_screen(&self) -> bool {
        self.get_video_visible()
    }
}

impl VideoHost for PipWindow {
    const SURFACE: Surface = Surface::Pip;
    fn video(&self) -> Video<'_> {
        self.global::<Video>()
    }
    fn video_on_screen(&self) -> bool {
        true
    }
}

thread_local! {
    /// Il router del thread dell'interfaccia, per gli eventi che arrivano dai thread di mpv.
    static ROUTER: RefCell<Weak<VideoRouter>> = const { RefCell::new(Weak::new()) };
}

/// Decide quale finestra mostra il video e tiene lo stato della riproduzione.
/// Vive sul thread dell'interfaccia.
pub struct VideoRouter {
    player: Rc<Player>,
    main: slint::Weak<MainWindow>,
    pip: slint::Weak<PipWindow>,
    /// Dove deve stare il video.
    target: Cell<Surface>,
    /// Chi ha il contesto di disegno di mpv adesso.
    holder: Cell<Option<Surface>>,
    /// Nascondere il mini player appena ha liberato il contesto.
    hide_pip_after_release: Cell<bool>,
    /// mpv ha (ri)avviato la riproduzione: il prossimo fotogramma disegnato rende `ready` vero.
    accept_frames: Cell<bool>,
    /// Flusso in riproduzione (serve per riconnettersi).
    stream: RefCell<Option<StreamInfo>>,
    /// Film o episodio: alla fine si ferma invece di riconnettersi.
    on_demand: Cell<bool>,
    /// Da dove partire al prossimo caricamento (secondi).
    start_secs: Cell<Option<u32>>,
    /// Ultima posizione letta: dopo un errore un film riparte da lì.
    last_position: Cell<f64>,
    /// Chiamata quando un film o un episodio arriva alla fine.
    on_finished: RefCell<Option<Box<dyn Fn()>>>,
    /// Fermato dall'utente (o mai partito): niente riconnessione.
    stopped: Cell<bool>,
    /// Riconnessione automatica (impostazione).
    reconnect: Cell<bool>,
    backoff: RefCell<Backoff>,
    retry_timer: slint::Timer,
    /// Schermo sempre acceso mentre c'è qualcosa in riproduzione.
    keep_awake: KeepAwake,
}

impl std::fmt::Debug for VideoRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoRouter")
            .field("target", &self.target.get())
            .field("holder", &self.holder.get())
            .finish_non_exhaustive()
    }
}

impl VideoRouter {
    pub fn new(player: Rc<Player>, main: &MainWindow, pip: &PipWindow) -> Rc<Self> {
        let router = Rc::new(Self {
            player,
            main: main.as_weak(),
            pip: pip.as_weak(),
            target: Cell::new(Surface::Main),
            holder: Cell::new(None),
            hide_pip_after_release: Cell::new(false),
            accept_frames: Cell::new(false),
            stream: RefCell::new(None),
            on_demand: Cell::new(false),
            start_secs: Cell::new(None),
            last_position: Cell::new(0.0),
            on_finished: RefCell::new(None),
            stopped: Cell::new(true),
            reconnect: Cell::new(true),
            backoff: RefCell::new(Backoff::default()),
            retry_timer: slint::Timer::default(),
            keep_awake: KeepAwake::default(),
        });
        ROUTER.with(|r| *r.borrow_mut() = Rc::downgrade(&router));
        router
    }

    /// Collega il video a una finestra. Da chiamare una volta per finestra, prima di mostrarla.
    pub fn install_main(
        self: &Rc<Self>,
        main: &MainWindow,
    ) -> Result<(), slint::SetRenderingNotifierError> {
        install(self, main)
    }

    /// Vedi [`Self::install_main`].
    pub fn install_pip(
        self: &Rc<Self>,
        pip: &PipWindow,
    ) -> Result<(), slint::SetRenderingNotifierError> {
        install(self, pip)
    }

    pub fn player(&self) -> &Player {
        &self.player
    }

    /// C'è qualcosa in riproduzione (anche durante una riconnessione).
    pub fn is_active(&self) -> bool {
        !self.stopped.get()
    }

    /// Imposta cosa fare quando un film o un episodio finisce.
    pub fn set_on_finished(&self, f: impl Fn() + 'static) {
        *self.on_finished.borrow_mut() = Some(Box::new(f));
    }

    /// Posizione corrente in secondi (e la ricorda per una eventuale ripresa dopo un errore).
    pub fn position(&self) -> Option<f64> {
        let position = self.player.position();
        if let Some(p) = position {
            self.last_position.set(p);
        }
        position
    }

    /// Accende o spegne la riconnessione automatica.
    pub fn set_reconnect(&self, enabled: bool) {
        self.reconnect.set(enabled);
    }

    /// Apre un flusso già risolto; `start`: secondi da cui partire (film ed episodi).
    pub fn play(&self, stream: StreamInfo, on_demand: bool, start: Option<u32>) {
        self.retry_timer.stop();
        self.backoff.borrow_mut().reset();
        self.set_attempt(0);
        self.stopped.set(false);
        self.on_demand.set(on_demand);
        self.start_secs.set(start);
        self.last_position.set(start.map_or(0.0, f64::from));
        *self.stream.borrow_mut() = Some(stream);
        // la pausa di mpv vale per il core, non per il file: un contenuto nuovo parte sempre
        if let Err(err) = self.player.set_paused(false) {
            log::warn!("{err}");
        }
        self.load_current();
    }

    fn load_current(&self) {
        let stream = self.stream.borrow();
        let Some(stream) = stream.as_ref() else {
            return;
        };
        let options = LoadOptions {
            user_agent: stream.user_agent.clone(),
            referrer: stream.referrer.clone(),
            start_secs: self.start_secs.get(),
        };
        if let Err(err) = self.player.load_with(&stream.url, &options) {
            log::error!("{err}");
            self.set_state(state::ERROR);
        }
    }

    /// Ferma la riproduzione.
    pub fn stop(&self) {
        self.retry_timer.stop();
        self.stopped.set(true);
        self.on_demand.set(false);
        *self.stream.borrow_mut() = None;
        self.set_attempt(0);
        if let Err(err) = self.player.stop() {
            log::error!("{err}");
        }
    }

    /// Il flusso è caduto: riprova dopo un'attesa crescente, o si arrende.
    fn retry_or_fail(self: &Rc<Self>) {
        if self.stopped.get() || !self.reconnect.get() || self.stream.borrow().is_none() {
            self.stopped.set(true);
            self.set_state(state::ERROR);
            return;
        }
        let next = self.backoff.borrow_mut().next_delay();
        let Some(delay) = next else {
            log::warn!(
                "riconnessione abbandonata dopo {} tentativi",
                Backoff::MAX_ATTEMPTS
            );
            self.stopped.set(true);
            self.set_state(state::ERROR);
            return;
        };
        let attempt = self.backoff.borrow().attempts();
        log::info!("riconnessione {attempt} tra {}s", delay.as_secs());
        self.set_attempt(i32::try_from(attempt).unwrap_or(i32::MAX));
        self.set_state(state::RECONNECTING);
        let weak = Rc::downgrade(self);
        self.retry_timer
            .start(slint::TimerMode::SingleShot, delay, move || {
                if let Some(router) = weak.upgrade() {
                    if !router.stopped.get() {
                        if router.on_demand.get() {
                            // un film riprende da dove si era interrotto
                            let at = router.last_position.get().max(0.0).floor() as u32;
                            router.start_secs.set((at > 0).then_some(at));
                        }
                        router.load_current();
                    }
                }
            });
    }

    /// Sposta il video nel mini player (che deve essere già visibile o in apertura).
    pub fn move_to_pip(&self) {
        self.hide_pip_after_release.set(false);
        self.retarget(Surface::Pip);
    }

    /// Riporta il video nella finestra principale; il mini player si nasconde quando ha liberato
    /// il contesto (o subito, se non lo aveva).
    pub fn move_to_main(&self) {
        if self.holder.get() == Some(Surface::Pip) {
            self.hide_pip_after_release.set(true);
        } else {
            self.hide_pip();
        }
        self.retarget(Surface::Main);
    }

    fn retarget(&self, target: Surface) {
        self.target.set(target);
        match self.holder.get() {
            // chi ha il contesto lo libera al prossimo fotogramma, poi passa la mano
            Some(holder) if holder != target => self.request_redraw(holder),
            _ => self.request_redraw(target),
        }
    }

    fn hide_pip(&self) {
        if let Some(pip) = self.pip.upgrade() {
            if let Err(err) = pip.hide() {
                log::error!("chiusura del mini player: {err}");
            }
        }
    }

    fn request_redraw(&self, surface: Surface) {
        match surface {
            Surface::Main => {
                if let Some(main) = self.main.upgrade() {
                    main.window().request_redraw();
                }
            }
            Surface::Pip => {
                if let Some(pip) = self.pip.upgrade() {
                    pip.window().request_redraw();
                }
            }
        }
    }

    /// Imposta `Video.state` in tutte e due le finestre.
    fn set_state(&self, value: i32) {
        // fermo o in errore lo schermo può spegnersi; negli altri stati si sta guardando
        self.keep_awake
            .set(!matches!(value, state::IDLE | state::ERROR));
        if let Some(main) = self.main.upgrade() {
            main.global::<Video>().set_state(value);
        }
        if let Some(pip) = self.pip.upgrade() {
            pip.global::<Video>().set_state(value);
        }
    }

    /// Imposta `Video.attempt` in tutte e due le finestre.
    fn set_attempt(&self, value: i32) {
        if let Some(main) = self.main.upgrade() {
            main.global::<Video>().set_attempt(value);
        }
        if let Some(pip) = self.pip.upgrade() {
            pip.global::<Video>().set_attempt(value);
        }
    }

    /// Nasconde il video (torna il segnaposto) in tutte e due le finestre.
    fn clear_ready(&self) {
        if let Some(main) = self.main.upgrade() {
            main.global::<Video>().set_ready(false);
        }
        if let Some(pip) = self.pip.upgrade() {
            pip.global::<Video>().set_ready(false);
        }
    }

    fn on_player_event(self: &Rc<Self>, event: PlayerEvent) {
        log::debug!("evento del player: {event:?}");
        match event {
            PlayerEvent::Loading => {
                self.accept_frames.set(false);
                self.clear_ready();
                // durante una riconnessione resta la scritta «Riconnessione…»
                if self.backoff.borrow().attempts() == 0 {
                    self.set_state(state::LOADING);
                }
            }
            PlayerEvent::Playing => {
                self.accept_frames.set(true);
                self.backoff.borrow_mut().reset();
                self.set_attempt(0);
                self.set_state(state::PLAYING);
                if let Some(holder) = self.holder.get() {
                    self.request_redraw(holder);
                }
            }
            PlayerEvent::Buffering(true) => self.set_state(state::BUFFERING),
            PlayerEvent::Buffering(false) => self.set_state(state::PLAYING),
            PlayerEvent::Ended(reason) => {
                self.accept_frames.set(false);
                self.clear_ready();
                match reason {
                    // per la TV in diretta anche la «fine» del flusso vuol dire che è caduto
                    EndReason::Error(message) => {
                        log::warn!("riproduzione interrotta: {message}");
                        self.retry_or_fail();
                    }
                    EndReason::Eof if self.on_demand.get() => {
                        self.stopped.set(true);
                        self.set_state(state::IDLE);
                        if let Some(finished) = self.on_finished.borrow().as_ref() {
                            finished();
                        }
                    }
                    EndReason::Eof => self.retry_or_fail(),
                    // un nuovo `load` sostituisce il flusso: lo stato lo decide il Loading che segue
                    EndReason::Stopped => {
                        if self.stopped.get() {
                            self.set_state(state::IDLE);
                        }
                    }
                }
            }
            PlayerEvent::Idle => {
                if self.stopped.get() {
                    self.clear_ready();
                }
            }
            PlayerEvent::Loaded | PlayerEvent::VideoSize { .. } => {}
        }
    }
}

/// Da chiamare dal thread di mpv: inoltra l'evento al router sul thread dell'interfaccia.
pub fn forward_player_event(event: PlayerEvent) {
    let sent = slint::invoke_from_event_loop(move || {
        ROUTER.with(|r| {
            if let Some(router) = r.borrow().upgrade() {
                router.on_player_event(event);
            }
        });
    });
    if sent.is_err() {
        // il ciclo degli eventi è già chiuso: l'app sta uscendo
        log::trace!("evento del player perso alla chiusura");
    }
}

/// Da chiamare dal thread di mpv quando c'è un nuovo fotogramma.
fn forward_frame_update() {
    let _ = slint::invoke_from_event_loop(|| {
        ROUTER.with(|r| {
            if let Some(router) = r.borrow().upgrade() {
                router.on_frame_update();
            }
        });
    });
}

impl VideoRouter {
    /// Ridisegna la finestra che ha il video, ma solo se il riquadro è sullo schermo: nelle
    /// pagine senza video (Guida, Impostazioni) l'audio continua senza ridisegnare a vuoto.
    fn on_frame_update(&self) {
        match self.holder.get() {
            Some(Surface::Main) => {
                if let Some(main) = self.main.upgrade() {
                    if main.video_on_screen() {
                        main.window().request_redraw();
                    }
                }
            }
            Some(Surface::Pip) => self.request_redraw(Surface::Pip),
            None => {}
        }
    }
}

fn install<T: VideoHost>(
    router: &Rc<VideoRouter>,
    host: &T,
) -> Result<(), slint::SetRenderingNotifierError> {
    let router = Rc::downgrade(router);
    let host_weak = host.as_weak();
    let mut video = WindowVideo::default();
    host.window()
        .set_rendering_notifier(move |rendering_state, graphics_api| {
            let (Some(router), Some(host)) = (router.upgrade(), host_weak.upgrade()) else {
                // l'app si sta chiudendo: libera comunque le risorse finché il contesto è corrente
                if matches!(rendering_state, RenderingState::RenderingTeardown) {
                    video.teardown();
                }
                return;
            };
            let GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                return;
            };
            match rendering_state {
                RenderingState::BeforeRendering => {
                    video.before_rendering(&router, &host, get_proc_address);
                }
                RenderingState::AfterRendering => video.after_rendering(),
                RenderingState::RenderingTeardown => {
                    video.teardown();
                    if router.holder.get() == Some(T::SURFACE) {
                        router.holder.set(None);
                    }
                }
                _ => {}
            }
        })
}

/// Stato del video di una finestra: risorse OpenGL del suo contesto e, se ce l'ha, il contesto
/// di disegno di mpv.
#[derive(Default)]
struct WindowVideo {
    // l'ordine conta: il contesto di mpv va liberato prima delle nostre risorse GL
    mpv: Option<RenderContext>,
    gl: Option<GlVideo>,
    rendered: bool,
    failed: bool,
}

impl WindowVideo {
    fn before_rendering<T: VideoHost>(
        &mut self,
        router: &VideoRouter,
        host: &T,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
    ) {
        self.rendered = false;
        if self.failed {
            return;
        }
        let me = T::SURFACE;
        let video = host.video();

        // 1. passaggio del contesto di mpv tra le finestre
        if self.mpv.is_some() && router.target.get() != me {
            self.mpv = None;
            router.holder.set(None);
            video.set_ready(false);
            log::debug!("contesto video liberato da {me:?}");
            if me == Surface::Pip && router.hide_pip_after_release.take() {
                let pip = router.pip.clone();
                // fuori dal notifier: nascondere una finestra mentre disegna non è sicuro
                slint::Timer::single_shot(Duration::ZERO, move || {
                    if let Some(pip) = pip.upgrade() {
                        if let Err(err) = pip.hide() {
                            log::error!("chiusura del mini player: {err}");
                        }
                    }
                });
            }
            router.request_redraw(router.target.get());
            return;
        }

        if self.gl.is_none() {
            match GlVideo::new(get_proc_address) {
                Ok(gl) => self.gl = Some(gl),
                Err(err) => {
                    log::error!("video disattivato in {me:?}: {err}");
                    self.failed = true;
                    return;
                }
            }
        }
        let Some(gl) = self.gl.as_mut() else { return };

        if self.mpv.is_none() && router.target.get() == me && router.holder.get().is_none() {
            // SAFETY: siamo nel rendering notifier di questa finestra, quindi il suo contesto
            // OpenGL è corrente e lo sarà in tutte le chiamate future al contesto di mpv, che
            // avvengono solo qui e in `teardown` (anche questo dentro il notifier). Il router
            // garantisce che non esista un altro RenderContext (holder è None).
            let created = unsafe {
                RenderContext::new(&router.player, get_proc_address, forward_frame_update)
            };
            match created {
                Ok(ctx) => {
                    log::debug!("contesto video creato in {me:?}");
                    self.mpv = Some(ctx);
                    router.holder.set(Some(me));
                    // l'uscita video precedente è andata persa: mpv ha spento la traccia video
                    if let Err(err) = router.player.reselect_video() {
                        log::warn!("{err}");
                    }
                }
                Err(err) => {
                    log::error!("{err}");
                    self.failed = true;
                    return;
                }
            }
        }
        let Some(mpv) = self.mpv.as_ref() else { return };

        // 2. dimensioni del riquadro in pixel fisici
        if !host.video_on_screen() {
            return;
        }
        let scale = host.window().scale_factor();
        let to_px = |logical: f32| (logical * scale).round() as i32;
        let (width, height) = (to_px(video.get_w()), to_px(video.get_h()));
        if width <= 0 || height <= 0 {
            return;
        }
        let corner = mask_corner(video.get_r() * scale, width, height);

        let resized = match gl.ensure_target(width, height) {
            Ok(resized) => resized,
            Err(err) => {
                log::error!("texture del video: {err}");
                self.failed = true;
                return;
            }
        };
        if resized {
            if let Some(image) = gl.image() {
                video.set_frame(image);
            }
        }

        // 3. disegno: solo se mpv ha un fotogramma nuovo o il riquadro è cambiato
        let new_frame = mpv.frame_ready();
        if !(new_frame || resized || corner != gl.corner) {
            return;
        }
        match gl.draw(mpv, get_proc_address, corner) {
            Ok(()) => {
                self.rendered = true;
                if new_frame && router.accept_frames.get() && !video.get_ready() {
                    video.set_ready(true);
                }
            }
            Err(err) => log::warn!("{err}"),
        }
    }

    fn after_rendering(&mut self) {
        if self.rendered {
            if let Some(mpv) = &self.mpv {
                mpv.report_swap();
            }
        }
    }

    fn teardown(&mut self) {
        // contesto di mpv prima, poi le nostre risorse: entrambi con il contesto GL corrente
        self.mpv = None;
        if let Some(mut gl) = self.gl.take() {
            gl.destroy();
        }
    }
}

/// Raggio della super-ellisse in pixel fisici, con lo stesso limite del componente `Sq`:
/// `p = min(1.5 r, 0.4 min(w, h))`.
fn mask_corner(radius_px: f32, width: i32, height: i32) -> f32 {
    let limit = width.min(height) as f32 * 0.4;
    (radius_px * 1.5).min(limit).max(0.0)
}

/// Errori OpenGL (testo di glow o del driver).
#[derive(Debug, thiserror::Error)]
enum GlError {
    #[error("OpenGL: {0}")]
    Gl(String),
    #[error("framebuffer incompleto (stato 0x{0:x})")]
    IncompleteFramebuffer(u32),
}

/// Risorse OpenGL di una finestra per il video: texture di destinazione, framebuffer e shader
/// della maschera degli angoli.
struct GlVideo {
    gl: glow::Context,
    program: glow::Program,
    vao: Option<glow::VertexArray>,
    vbo: glow::Buffer,
    u_size: Option<glow::UniformLocation>,
    u_corner: Option<glow::UniformLocation>,
    texture: Option<glow::Texture>,
    fbo: Option<glow::Framebuffer>,
    size: (i32, i32),
    /// Raggio della maschera già applicata alla texture.
    corner: f32,
    internal_format: i32,
}

/// Varianti del linguaggio degli shader secondo il contesto creato da Slint.
struct GlslFlavor {
    vertex: &'static str,
    fragment: &'static str,
}

fn glsl_flavor(version: &glow::Version) -> GlslFlavor {
    match (version.is_embedded, version.major) {
        (true, 3..) => GlslFlavor {
            vertex: "#version 300 es\n#define ATTR in\n",
            fragment: "#version 300 es\nprecision highp float;\nout vec4 frag_color;\n#define FRAG_COLOR frag_color\n",
        },
        (true, _) => GlslFlavor {
            vertex: "#version 100\n#define ATTR attribute\n",
            fragment: "#version 100\nprecision highp float;\n#define FRAG_COLOR gl_FragColor\n",
        },
        (false, 4..) => GlslFlavor {
            vertex: "#version 150\n#define ATTR in\n",
            fragment: "#version 150\nout vec4 frag_color;\n#define FRAG_COLOR frag_color\n",
        },
        (false, 3) if version.minor >= 2 => GlslFlavor {
            vertex: "#version 150\n#define ATTR in\n",
            fragment: "#version 150\nout vec4 frag_color;\n#define FRAG_COLOR frag_color\n",
        },
        (false, _) => GlslFlavor {
            vertex: "#version 120\n#define ATTR attribute\n",
            fragment: "#version 120\n#define FRAG_COLOR gl_FragColor\n",
        },
    }
}

const VERTEX_SHADER: &str = "
ATTR vec2 a_pos;
void main() { gl_Position = vec4(a_pos, 0.0, 1.0); }
";

/// Copertura della super-ellisse di `Sq`. Ogni angolo è la curva di Bézier cubica
/// (0,p) → (0,q), (q,0) → (p,0) con q = p/4, in coordinate che partono dall'angolo.
/// L'uscita moltiplica l'alfa della texture (vedi `blend_func_separate` in `draw`).
const FRAGMENT_SHADER: &str = "
uniform vec2 u_size;
uniform float u_corner;

// y della curva per x = s (0 <= s <= p); x(t) cresce con t, quindi basta una bisezione
float curve(float s) {
    float p = u_corner;
    float q = 0.25 * p;
    float lo = 0.0;
    float hi = 1.0;
    for (int i = 0; i < 18; i++) {
        float t = 0.5 * (lo + hi);
        float x = 3.0 * (1.0 - t) * t * t * q + t * t * t * p;
        if (x < s) { lo = t; } else { hi = t; }
    }
    float t = 0.5 * (lo + hi);
    float it = 1.0 - t;
    return it * it * it * p + 3.0 * it * it * t * q;
}

void main() {
    vec2 c = gl_FragCoord.xy;
    vec2 d = min(c, u_size - c);
    float coverage = 1.0;
    if (u_corner > 0.0 && d.x < u_corner && d.y < u_corner) {
        // distanze verticale e orizzontale dalla curva (simmetrica rispetto alla diagonale);
        // la distanza vera è circa il loro prodotto diviso l'ipotenusa
        float dv = d.y - curve(d.x);
        float du = d.x - curve(d.y);
        float dist = sign(dv) * abs(dv * du) / max(sqrt(dv * dv + du * du), 0.0001);
        coverage = clamp(dist + 0.5, 0.0, 1.0);
    }
    FRAG_COLOR = vec4(0.0, 0.0, 0.0, coverage);
}
";

impl GlVideo {
    fn new(get_proc_address: &dyn Fn(&CStr) -> *const c_void) -> Result<Self, GlError> {
        // SAFETY: chiamata dal rendering notifier, con il contesto della finestra corrente;
        // `get_proc_address` risolve i simboli di quel contesto.
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };
        let version = gl.version().clone();
        // SAFETY: contesto corrente (vedi sopra); stringhe statiche.
        let renderer = unsafe { gl.get_parameter_string(glow::RENDERER) };
        log::info!(
            "OpenGL {}{}.{} su {renderer}",
            if version.is_embedded { "ES " } else { "" },
            version.major,
            version.minor
        );
        let flavor = glsl_flavor(&version);
        let has_vao = version.major >= 3;
        let internal_format = if version.is_embedded && version.major < 3 {
            glow::RGBA as i32
        } else {
            glow::RGBA8 as i32
        };

        // SAFETY: contesto corrente; tutti gli oggetti creati qui appartengono a questo contesto e
        // vengono distrutti in `destroy`, chiamata con lo stesso contesto corrente.
        unsafe {
            let program = gl.create_program().map_err(GlError::Gl)?;
            let shaders = [
                (
                    glow::VERTEX_SHADER,
                    format!("{}{VERTEX_SHADER}", flavor.vertex),
                ),
                (
                    glow::FRAGMENT_SHADER,
                    format!("{}{FRAGMENT_SHADER}", flavor.fragment),
                ),
            ];
            let mut compiled = Vec::with_capacity(shaders.len());
            for (kind, source) in &shaders {
                let shader = gl.create_shader(*kind).map_err(GlError::Gl)?;
                gl.shader_source(shader, source);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    gl.delete_program(program);
                    return Err(GlError::Gl(format!("shader della maschera: {log}")));
                }
                gl.attach_shader(program, shader);
                compiled.push(shader);
            }
            gl.bind_attrib_location(program, 0, "a_pos");
            gl.link_program(program);
            for shader in compiled {
                gl.detach_shader(program, shader);
                gl.delete_shader(shader);
            }
            if !gl.get_program_link_status(program) {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(GlError::Gl(format!("programma della maschera: {log}")));
            }
            let u_size = gl.get_uniform_location(program, "u_size");
            let u_corner = gl.get_uniform_location(program, "u_corner");

            let vbo = gl.create_buffer().map_err(GlError::Gl)?;
            let quad: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            let bytes: Vec<u8> = quad.iter().flat_map(|v| v.to_ne_bytes()).collect();
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, &bytes, glow::STATIC_DRAW);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);

            let vao = if has_vao {
                Some(gl.create_vertex_array().map_err(GlError::Gl)?)
            } else {
                None
            };

            Ok(Self {
                gl,
                program,
                vao,
                vbo,
                u_size,
                u_corner,
                texture: None,
                fbo: None,
                size: (0, 0),
                corner: -1.0,
                internal_format,
            })
        }
    }

    /// Prepara texture e framebuffer di `width` × `height`; `true` se sono stati (ri)creati.
    fn ensure_target(&mut self, width: i32, height: i32) -> Result<bool, GlError> {
        if self.texture.is_some() && self.size == (width, height) {
            return Ok(false);
        }
        let gl = &self.gl;
        // SAFETY: contesto corrente (siamo nel notifier); gli oggetti vecchi sono di questo contesto.
        // Il binding del framebuffer e della texture viene salvato e ripristinato.
        unsafe {
            if let Some(fbo) = self.fbo.take() {
                gl.delete_framebuffer(fbo);
            }
            if let Some(texture) = self.texture.take() {
                gl.delete_texture(texture);
            }
            let previous_fbo = current_framebuffer(gl);

            let texture = gl.create_texture().map_err(GlError::Gl)?;
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MAG_FILTER,
                glow::LINEAR as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                self.internal_format,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);

            let fbo = gl.create_framebuffer().map_err(GlError::Gl)?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            // finché non arriva il primo fotogramma la texture è nera e opaca
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.viewport(0, 0, width, height);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.bind_framebuffer(glow::FRAMEBUFFER, previous_fbo);

            self.texture = Some(texture);
            self.fbo = Some(fbo);
            self.size = (width, height);
            self.corner = -1.0;
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(GlError::IncompleteFramebuffer(status));
            }
        }
        Ok(true)
    }

    /// L'immagine Slint che mostra la texture.
    fn image(&self) -> Option<slint::Image> {
        let texture = self.texture?;
        let (width, height) = (
            u32::try_from(self.size.0).ok()?,
            u32::try_from(self.size.1).ok()?,
        );
        // SAFETY: la texture è RGBA, 2D, di queste dimensioni e appartiene al contesto della
        // finestra che la mostra; resta valida finché non la sostituiamo, e quando lo facciamo
        // impostiamo subito la nuova immagine (nello stesso notifier, prima che Slint disegni).
        let image = unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                texture.0,
                (width, height).into(),
            )
        }
        .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
        .build();
        Some(image)
    }

    /// Disegna il fotogramma di mpv nella texture e ritaglia gli angoli.
    fn draw(
        &mut self,
        mpv: &RenderContext,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        corner: f32,
    ) -> Result<(), sintonia_player::PlayerError> {
        let Some(fbo) = self.fbo else { return Ok(()) };
        let gl = &self.gl;
        let (width, height) = self.size;

        // SAFETY: contesto corrente (siamo nel notifier). Salviamo lo stato che FemtoVG si aspetta
        // di ritrovare e portiamo il resto ai valori standard che mpv richiede (render_gl.h).
        let saved = unsafe { SavedState::capture(gl) };
        // SAFETY: come sopra.
        unsafe { reset_to_defaults(gl, self.vao.is_some()) };

        let result = mpv.render(get_proc_address, fbo.0.get(), width, height, false);

        if result.is_ok() && corner > 0.0 {
            // SAFETY: contesto corrente; programma, buffer e framebuffer sono di questo contesto.
            unsafe {
                gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                gl.viewport(0, 0, width, height);
                gl.use_program(Some(self.program));
                gl.bind_vertex_array(self.vao);
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
                gl.enable_vertex_attrib_array(0);
                gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 0, 0);
                gl.uniform_2_f32(self.u_size.as_ref(), width as f32, height as f32);
                gl.uniform_1_f32(self.u_corner.as_ref(), corner);
                // colore invariato, alfa moltiplicato per la copertura
                gl.enable(glow::BLEND);
                gl.blend_func_separate(glow::ZERO, glow::ONE, glow::ZERO, glow::SRC_ALPHA);
                gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
                gl.disable(glow::BLEND);
                gl.disable_vertex_attrib_array(0);
                gl.bind_buffer(glow::ARRAY_BUFFER, None);
                gl.bind_vertex_array(None);
                gl.use_program(None);
            }
        }
        // SAFETY: contesto corrente; ripristina quanto salvato sopra.
        unsafe { saved.restore(gl) };

        result.map(|()| self.corner = corner)
    }

    fn destroy(&mut self) {
        let gl = &self.gl;
        // SAFETY: chiamata da `teardown` dentro il notifier, con il contesto corrente; ogni oggetto
        // viene distrutto una sola volta (take).
        unsafe {
            if let Some(fbo) = self.fbo.take() {
                gl.delete_framebuffer(fbo);
            }
            if let Some(texture) = self.texture.take() {
                gl.delete_texture(texture);
            }
            if let Some(vao) = self.vao.take() {
                gl.delete_vertex_array(vao);
            }
            gl.delete_buffer(self.vbo);
            gl.delete_program(self.program);
        }
    }
}

/// Framebuffer legato adesso (`None` = quello della finestra).
///
/// # Safety
/// Il contesto di `gl` deve essere corrente.
unsafe fn current_framebuffer(gl: &glow::Context) -> Option<glow::Framebuffer> {
    // SAFETY: garantito dal chiamante.
    let id = unsafe { gl.get_parameter_i32(glow::FRAMEBUFFER_BINDING) };
    u32::try_from(id)
        .ok()
        .and_then(NonZeroU32::new)
        .map(glow::NativeFramebuffer)
}

/// Lo stato OpenGL di FemtoVG che tocchiamo.
struct SavedState {
    framebuffer: Option<glow::Framebuffer>,
    viewport: [i32; 4],
}

impl SavedState {
    /// # Safety
    /// Il contesto di `gl` deve essere corrente.
    unsafe fn capture(gl: &glow::Context) -> Self {
        let mut viewport = [0; 4];
        // SAFETY: garantito dal chiamante.
        unsafe {
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            Self {
                framebuffer: current_framebuffer(gl),
                viewport,
            }
        }
    }

    /// # Safety
    /// Il contesto di `gl` deve essere corrente.
    unsafe fn restore(self, gl: &glow::Context) {
        let [x, y, w, h] = self.viewport;
        // SAFETY: garantito dal chiamante.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            gl.viewport(x, y, w, h);
        }
    }
}

/// Porta lo stato ai valori standard di OpenGL che mpv si aspetta. FemtoVG reimposta da sé tutto
/// ciò che gli serve all'inizio di ogni disegno, tranne framebuffer e viewport (vedi
/// [`SavedState`]).
///
/// # Safety
/// Il contesto di `gl` deve essere corrente.
unsafe fn reset_to_defaults(gl: &glow::Context, has_vao: bool) {
    // SAFETY: garantito dal chiamante.
    unsafe {
        gl.disable(glow::BLEND);
        gl.disable(glow::SCISSOR_TEST);
        gl.disable(glow::STENCIL_TEST);
        gl.disable(glow::DEPTH_TEST);
        gl.disable(glow::CULL_FACE);
        gl.color_mask(true, true, true, true);
        gl.active_texture(glow::TEXTURE1);
        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, None);
        gl.use_program(None);
        if has_vao {
            gl.bind_vertex_array(None);
        }
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_matches_sq_component() {
        // Sq: p = min(r * 1.5, min(w, h) * 0.4)
        assert_eq!(mask_corner(22.0, 466, 262), 33.0);
        assert_eq!(mask_corner(100.0, 120, 80), 32.0);
        assert_eq!(mask_corner(0.0, 100, 100), 0.0);
    }

    #[test]
    fn glsl_flavors() {
        let desktop = glow::Version {
            major: 4,
            minor: 6,
            is_embedded: false,
            revision: None,
            vendor_info: String::new(),
        };
        assert!(glsl_flavor(&desktop).fragment.starts_with("#version 150"));
        let es2 = glow::Version {
            major: 2,
            minor: 0,
            is_embedded: true,
            revision: None,
            vendor_info: String::new(),
        };
        assert!(glsl_flavor(&es2).fragment.starts_with("#version 100"));
    }
}
