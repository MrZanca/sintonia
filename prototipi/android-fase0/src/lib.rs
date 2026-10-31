//! Prova di fattibilità di M5 fase 0: una finestra Slint con un pulsante e il video di mpv
//! disegnato dentro il *rendering notifier*, come fa `sintonia-app/src/video.rs` su Windows.
//!
//! Non è l'app: serve a misurare avvio, fotogrammi e decodifica hardware sul telefono. Il flusso e
//! le opzioni si leggono da `prova.txt` nella cartella dati dell'app (prima riga: indirizzo; poi
//! righe `hwdec=...`, `live=no` per provare un film, `opt=nome=valore` per un'opzione di mpv), così si
//! provano più casi senza ricompilare.
//!
//! In fase 2 è il banco di prova di `sintonia-player` su Android: JavaVM, opzioni predefinite e
//! ciclo di vita (`sintonia_player::lifecycle`), con le stesse regole di `VideoRouter::suspend`.

use std::cell::{Cell, RefCell};
use std::ffi::{c_void, CStr};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};

use android_activity::{AndroidApp, MainEvent, PollEvent};
use glow::HasContext;
use sintonia_player::lifecycle::{Lifecycle, LifecycleEvent, Transition};
use sintonia_player::{LoadOptions, Player, PlayerConfig, PlayerEvent, RenderContext};
use slint::{ComponentHandle, GraphicsAPI, RenderingState};

slint::include_modules!();

/// Modalità di decodifica provate dal secondo pulsante.
const MODES: [&str; 5] = [
    sintonia_player::HWDEC,
    "auto-safe",
    "mediacodec",
    "mediacodec-copy",
    "no",
];

thread_local! {
    /// Ciclo di vita e cosa fare alle transizioni; l'ascoltatore di Slint parte prima della prova.
    static LIFECYCLE: RefCell<(Lifecycle, Option<Rc<Prova>>)> = RefCell::default();
}

#[derive(Default)]
struct Counters {
    /// Chiamate del notifier (fotogrammi dell'interfaccia) e fotogrammi video disegnati nel secondo.
    ui_frames: Cell<u32>,
    video_frames: Cell<u32>,
    /// Intervallo più lungo tra due fotogrammi video nel secondo (ms).
    max_gap_ms: Cell<u32>,
    last_video: Cell<Option<Instant>>,
    /// Quando è partito il caricamento; serve a misurare l'attesa del primo fotogramma.
    load_at: Cell<Option<Instant>>,
    first_frame_logged: Cell<bool>,
}

struct Prova {
    player: Rc<Player>,
    url: String,
    mode: Cell<usize>,
    counters: Counters,
    started: Instant,
    /// Diretta (`true`) o film: decide cosa fare quando l'app va in secondo piano.
    live: bool,
    /// Sospesa dal ciclo di vita; per un film: se era in riproduzione.
    suspended: Cell<Option<bool>>,
}

/// Registra un segnale del ciclo di vita e sospende o riprende la prova.
fn on_lifecycle(event: &PollEvent<'_>) {
    let event = match event {
        PollEvent::Main(MainEvent::Start) => LifecycleEvent::Started,
        PollEvent::Main(MainEvent::Stop) => LifecycleEvent::Stopped,
        PollEvent::Main(MainEvent::InitWindow { .. }) => LifecycleEvent::WindowCreated,
        PollEvent::Main(MainEvent::TerminateWindow { .. }) => LifecycleEvent::WindowDestroyed,
        PollEvent::Main(MainEvent::GainedFocus) => {
            log::info!("ciclo di vita: focus preso");
            return;
        }
        PollEvent::Main(MainEvent::LostFocus) => {
            log::info!("ciclo di vita: focus perso");
            return;
        }
        _ => return,
    };
    let (transition, state) = LIFECYCLE.with(|l| {
        let mut l = l.borrow_mut();
        (l.0.handle(event), l.1.clone())
    });
    log::info!("ciclo di vita: {event:?} -> {transition:?}");
    if let (Some(transition), Some(state)) = (transition, state) {
        state.on_transition(transition);
    }
}

#[no_mangle]
fn android_main(app: AndroidApp) {
    let started = Instant::now();
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("sintonia"),
    );
    std::panic::set_hook(Box::new(|info| log::error!("PANIC: {info}")));

    // SAFETY: `vm_as_ptr` è la JavaVM del processo, valida per tutta la sua vita.
    if let Err(err) = unsafe { sintonia_player::set_java_vm(app.vm_as_ptr()) } {
        log::error!("{err}");
    }

    let data_dir = app.internal_data_path();
    let tls_ca_file = data_dir.as_deref().and_then(|dir| {
        sintonia_player::tls::system_ca_bundle(dir)
            .map_err(|err| log::error!("certificati di sistema: {err}"))
            .ok()
    });
    let config = data_dir
        .as_deref()
        .and_then(|dir| std::fs::read_to_string(dir.join("prova.txt")).ok())
        .unwrap_or_default();
    let mut lines = config.lines().map(str::trim).filter(|l| !l.is_empty());
    let url = lines.next().unwrap_or("").to_owned();
    let rest: Vec<&str> = lines.collect();
    let mode = rest
        .iter()
        .find_map(|l| l.strip_prefix("hwdec="))
        .and_then(|m| MODES.iter().position(|x| *x == m))
        .unwrap_or(0);
    let live = !rest.contains(&"live=no");
    let options: Vec<(&str, &str)> = rest
        .iter()
        .filter_map(|l| l.strip_prefix("opt=")?.split_once('='))
        .collect();
    log::info!(
        "mpv API {:?}, modalità hwdec {}, {}, opzioni {options:?}",
        sintonia_player::api_version(),
        MODES[mode],
        if live { "diretta" } else { "film" }
    );

    // come farà l'app con il video in corso (fase 3): lo schermo resta acceso nelle prove lunghe
    app.set_window_flags(
        android_activity::WindowManagerFlags::KEEP_SCREEN_ON,
        android_activity::WindowManagerFlags::empty(),
    );
    if let Err(err) = slint::android::init_with_event_listener(app, on_lifecycle) {
        log::error!("backend Android di Slint: {err}");
        return;
    }

    let player_config = PlayerConfig {
        log_level: "info",
        tls_ca_file,
        ..PlayerConfig::default()
    };
    let player = match Player::new(&player_config, |event| {
        if matches!(
            event,
            PlayerEvent::Loaded
                | PlayerEvent::Playing
                | PlayerEvent::VideoSize { .. }
                | PlayerEvent::Ended(_)
        ) {
            log::info!("evento del player: {event:?}");
        }
    }) {
        Ok(player) => Rc::new(player),
        Err(err) => {
            log::error!("player: {err}");
            return;
        }
    };

    for (name, value) in options {
        if let Err(err) = player.set_option(name, value) {
            log::warn!("{name}={value}: {err}");
        }
    }

    let state = Rc::new(Prova {
        player,
        url,
        mode: Cell::new(mode),
        counters: Counters::default(),
        started,
        live,
        suspended: Cell::new(None),
    });
    LIFECYCLE.with(|l| l.borrow_mut().1 = Some(Rc::clone(&state)));
    if let Err(err) = run(&state) {
        log::error!("{err}");
    }
}

fn run(state: &Rc<Prova>) -> Result<(), Box<dyn std::error::Error>> {
    let ui = ProvaWindow::new()?;
    ui.set_mode(MODES[state.mode.get()].into());

    let weak = Rc::downgrade(state);
    let ui_weak = ui.as_weak();
    ui.on_next_mode(move || {
        let (Some(state), Some(ui)) = (weak.upgrade(), ui_weak.upgrade()) else {
            return;
        };
        let next = (state.mode.get() + 1) % MODES.len();
        state.mode.set(next);
        ui.set_mode(MODES[next].into());
        state.start_playback();
    });

    install_notifier(state, &ui)?;

    let stats_state = Rc::clone(state);
    let stats_ui = ui.as_weak();
    let timer = slint::Timer::default();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(1),
        move || {
            if let Some(ui) = stats_ui.upgrade() {
                let text = stats_state.stats_line();
                log::info!("{text}");
                ui.set_stats(text.into());
            }
        },
    );

    ui.run()?;
    Ok(())
}

impl Prova {
    /// Come `VideoRouter::suspend` e `resume` dell'app: il film va in pausa, la diretta si ferma e
    /// al ritorno si riapre.
    fn on_transition(&self, transition: Transition) {
        match transition {
            Transition::Suspend => {
                let was_playing = !self.player.is_paused();
                let result = if self.live {
                    self.player.stop()
                } else {
                    self.player.set_paused(true)
                };
                if let Err(err) = result {
                    log::warn!("{err}");
                }
                self.suspended.set(Some(was_playing));
                log::info!(
                    "sospesa ({})",
                    if self.live {
                        "diretta fermata"
                    } else {
                        "pausa"
                    }
                );
            }
            Transition::Resume => {
                let Some(was_playing) = self.suspended.take() else {
                    return;
                };
                if self.live {
                    self.start_playback();
                } else if was_playing {
                    if let Err(err) = self.player.set_paused(false) {
                        log::warn!("{err}");
                    }
                }
                log::info!("ripresa");
            }
        }
    }

    fn start_playback(&self) {
        let mode = MODES[self.mode.get()];
        if let Err(err) = self.player.set_option("hwdec", mode) {
            log::warn!("hwdec={mode}: {err}");
        }
        if self.url.is_empty() {
            log::warn!("prova.txt senza indirizzo: niente da riprodurre");
            return;
        }
        self.counters.load_at.set(Some(Instant::now()));
        self.counters.first_frame_logged.set(false);
        if let Err(err) = self.player.set_paused(false) {
            log::warn!("{err}");
        }
        if let Err(err) = self.player.load_with(&self.url, &LoadOptions::default()) {
            log::error!("load: {err}");
        }
    }

    /// Riga di numeri del secondo appena trascorso (mai l'indirizzo del flusso).
    fn stats_line(&self) -> String {
        let c = &self.counters;
        let get = |name: &str| self.player.property(name).unwrap_or_else(|| "-".into());
        format!(
            "hwdec={} (richiesto {}) codec={} {}x{} @{} fps | UI {}/s video {}/s gap max {} ms | \
             persi: vo {} decoder {} ritardati {} | avsync {}",
            get("hwdec-current"),
            MODES[self.mode.get()],
            get("video-codec"),
            get("video-params/w"),
            get("video-params/h"),
            get("container-fps"),
            c.ui_frames.replace(0),
            c.video_frames.replace(0),
            c.max_gap_ms.replace(0),
            get("frame-drop-count"),
            get("decoder-frame-drop-count"),
            get("vo-delayed-frame-count"),
            get("avsync"),
        )
    }
}

fn install_notifier(
    state: &Rc<Prova>,
    ui: &ProvaWindow,
) -> Result<(), slint::SetRenderingNotifierError> {
    let state = Rc::clone(state);
    let ui_weak = ui.as_weak();
    let mut video = VideoSide::default();
    ui.window()
        .set_rendering_notifier(move |rendering_state, graphics_api| {
            let GraphicsAPI::NativeOpenGL { get_proc_address } = graphics_api else {
                return;
            };
            let Some(ui) = ui_weak.upgrade() else { return };
            match rendering_state {
                RenderingState::BeforeRendering => {
                    state
                        .counters
                        .ui_frames
                        .set(state.counters.ui_frames.get() + 1);
                    video.before_rendering(&state, &ui, get_proc_address);
                }
                RenderingState::AfterRendering => {
                    if video.rendered {
                        if let Some(mpv) = &video.mpv {
                            mpv.report_swap();
                        }
                    }
                }
                RenderingState::RenderingTeardown => {
                    // Android distrugge la superficie in secondo piano: il contesto GL se ne va
                    log::info!("teardown del rendering (superficie persa)");
                    video.teardown();
                }
                _ => {}
            }
        })
}

#[derive(Default)]
struct VideoSide {
    // l'ordine conta: il contesto di mpv va liberato prima delle risorse GL
    mpv: Option<RenderContext>,
    gl: Option<GlTarget>,
    rendered: bool,
    failed: bool,
    recreated: bool,
}

impl VideoSide {
    fn before_rendering(
        &mut self,
        state: &Rc<Prova>,
        ui: &ProvaWindow,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
    ) {
        self.rendered = false;
        if self.failed {
            return;
        }
        if self.gl.is_none() {
            match GlTarget::new(get_proc_address) {
                Ok(gl) => self.gl = Some(gl),
                Err(err) => {
                    log::error!("OpenGL: {err}");
                    self.failed = true;
                    return;
                }
            }
        }
        let Some(gl) = self.gl.as_mut() else { return };

        if self.mpv.is_none() {
            let weak = ui.as_weak();
            // SAFETY: siamo nel rendering notifier, quindi il contesto OpenGL della finestra è
            // corrente adesso e lo sarà in ogni chiamata successiva a questo oggetto, che avviene
            // solo qui e in `teardown` (anch'esso nel notifier). Esiste un solo RenderContext.
            let created = unsafe {
                RenderContext::new(&state.player, get_proc_address, move || {
                    let weak = weak.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak.upgrade() {
                            ui.window().request_redraw();
                        }
                    });
                })
            };
            match created {
                Ok(ctx) => {
                    log::info!(
                        "contesto video creato, {} ms dall'avvio del processo",
                        state.started.elapsed().as_millis()
                    );
                    self.mpv = Some(ctx);
                    if self.recreated {
                        // dopo il ritorno in primo piano la traccia video va riaccesa
                        if let Err(err) = state.player.reselect_video() {
                            log::warn!("{err}");
                        }
                    } else {
                        state.start_playback();
                    }
                    self.recreated = true;
                }
                Err(err) => {
                    log::error!("{err}");
                    self.failed = true;
                    return;
                }
            }
        }
        let Some(mpv) = self.mpv.as_ref() else { return };

        let scale = ui.window().scale_factor();
        let to_px = |logical: f32| (logical * scale).round() as i32;
        let (width, height) = (to_px(ui.get_video_w()), to_px(ui.get_video_h()));
        if width <= 0 || height <= 0 {
            return;
        }
        let resized = match gl.ensure_target(width, height) {
            Ok(resized) => resized,
            Err(err) => {
                log::error!("texture: {err}");
                self.failed = true;
                return;
            }
        };
        if resized {
            if let Some(image) = gl.image() {
                ui.set_frame(image);
            }
        }
        let new_frame = mpv.frame_ready();
        if !(new_frame || resized) {
            return;
        }
        if let Err(err) = gl.draw(mpv, get_proc_address) {
            log::warn!("{err}");
            return;
        }
        self.rendered = true;
        if new_frame {
            let counters = &state.counters;
            let now = Instant::now();
            counters.video_frames.set(counters.video_frames.get() + 1);
            if let Some(prev) = counters.last_video.replace(Some(now)) {
                let gap = u32::try_from(now.duration_since(prev).as_millis()).unwrap_or(u32::MAX);
                counters.max_gap_ms.set(counters.max_gap_ms.get().max(gap));
            }
            if !counters.first_frame_logged.replace(true) {
                if let Some(at) = counters.load_at.get() {
                    log::info!(
                        "primo fotogramma {} ms dopo il caricamento",
                        at.elapsed().as_millis()
                    );
                }
                ui.set_ready(true);
            }
        }
    }

    fn teardown(&mut self) {
        self.mpv = None;
        if let Some(mut gl) = self.gl.take() {
            gl.destroy();
        }
    }
}

/// Texture e framebuffer in cui mpv disegna; la texture diventa un'immagine Slint.
struct GlTarget {
    gl: glow::Context,
    texture: Option<glow::Texture>,
    fbo: Option<glow::Framebuffer>,
    size: (i32, i32),
    internal_format: i32,
}

impl GlTarget {
    fn new(get_proc_address: &dyn Fn(&CStr) -> *const c_void) -> Result<Self, String> {
        // SAFETY: chiamata dal rendering notifier, con il contesto della finestra corrente.
        let gl = unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) };
        let version = gl.version().clone();
        // SAFETY: contesto corrente; sono stringhe del driver.
        let (renderer, vendor) = unsafe {
            (
                gl.get_parameter_string(glow::RENDERER),
                gl.get_parameter_string(glow::VENDOR),
            )
        };
        log::info!(
            "OpenGL {}{}.{} su {renderer} ({vendor})",
            if version.is_embedded { "ES " } else { "" },
            version.major,
            version.minor
        );
        let internal_format = if version.is_embedded && version.major < 3 {
            glow::RGBA as i32
        } else {
            glow::RGBA8 as i32
        };
        Ok(Self {
            gl,
            texture: None,
            fbo: None,
            size: (0, 0),
            internal_format,
        })
    }

    fn ensure_target(&mut self, width: i32, height: i32) -> Result<bool, String> {
        if self.texture.is_some() && self.size == (width, height) {
            return Ok(false);
        }
        let gl = &self.gl;
        // SAFETY: contesto corrente (siamo nel notifier); gli oggetti vecchi sono di questo
        // contesto. Il framebuffer legato viene salvato e ripristinato.
        unsafe {
            if let Some(fbo) = self.fbo.take() {
                gl.delete_framebuffer(fbo);
            }
            if let Some(texture) = self.texture.take() {
                gl.delete_texture(texture);
            }
            let saved = SavedGl::capture(gl);
            let texture = gl.create_texture()?;
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            for (param, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::LINEAR),
                (glow::TEXTURE_MAG_FILTER, glow::LINEAR),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, param, value as i32);
            }
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
            let fbo = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(texture),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.viewport(0, 0, width, height);
            gl.clear(glow::COLOR_BUFFER_BIT);
            saved.restore(gl);
            self.texture = Some(texture);
            self.fbo = Some(fbo);
            self.size = (width, height);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("framebuffer incompleto (0x{status:x})"));
            }
        }
        Ok(true)
    }

    fn image(&self) -> Option<slint::Image> {
        let texture = self.texture?;
        let (w, h) = (
            u32::try_from(self.size.0).ok()?,
            u32::try_from(self.size.1).ok()?,
        );
        // SAFETY: la texture è RGBA, 2D, di queste dimensioni e del contesto della finestra che la
        // mostra; viene sostituita nello stesso notifier prima che Slint disegni.
        let image = unsafe {
            slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(texture.0, (w, h).into())
        }
        .origin(slint::BorrowedOpenGLTextureOrigin::TopLeft)
        .build();
        Some(image)
    }

    fn draw(
        &mut self,
        mpv: &RenderContext,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
    ) -> Result<(), sintonia_player::PlayerError> {
        let Some(fbo) = self.fbo else { return Ok(()) };
        let gl = &self.gl;
        let (width, height) = self.size;
        // SAFETY: contesto corrente; salva tutto lo stato che FemtoVG si aspetta di ritrovare, porta
        // il resto ai valori standard richiesti da mpv (render_gl.h), poi ripristina.
        let saved = unsafe {
            let saved = SavedGl::capture(gl);
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
            gl.bind_vertex_array(None);
            gl.bind_buffer(glow::ARRAY_BUFFER, None);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.stencil_mask(0xff);
            gl.depth_mask(true);
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.viewport(0, 0, width, height);
            saved
        };
        let result = mpv.render(get_proc_address, fbo.0.get(), width, height, false);
        // SAFETY: contesto corrente; ripristina quanto salvato sopra.
        unsafe { saved.restore(gl) };
        result
    }

    fn destroy(&mut self) {
        let gl = &self.gl;
        // SAFETY: chiamata dal teardown nel notifier con il contesto corrente; ogni oggetto
        // viene distrutto una volta sola (take).
        unsafe {
            if let Some(fbo) = self.fbo.take() {
                gl.delete_framebuffer(fbo);
            }
            if let Some(texture) = self.texture.take() {
                gl.delete_texture(texture);
            }
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

/// Lo stato OpenGL che FemtoVG dà per scontato tra un disegno e l'altro. Su Android FemtoVG
/// non lo reimposta tutto da sé: bastano una texture scollegata o un programma cambiato per
/// avere testo a blocchi neri e interfaccia sparita.
struct SavedGl {
    framebuffer: Option<glow::Framebuffer>,
    viewport: [i32; 4],
    active_texture: u32,
    textures: [Option<glow::Texture>; 2],
    program: Option<glow::Program>,
    vertex_array: Option<glow::VertexArray>,
    array_buffer: Option<glow::Buffer>,
    enabled: [(u32, bool); 5],
    color_mask: [i32; 4],
    blend_func: [i32; 4],
    blend_equation: [i32; 2],
    unpack_alignment: i32,
    stencil_mask: i32,
    stencil_func: [i32; 3],
    stencil_ops: [i32; 3],
    depth_mask: bool,
    depth_func: i32,
    scissor: [i32; 4],
    clear_color: [f32; 4],
}

impl SavedGl {
    /// # Safety
    /// Il contesto di `gl` deve essere corrente.
    unsafe fn capture(gl: &glow::Context) -> Self {
        let nz = |id: i32| u32::try_from(id).ok().and_then(NonZeroU32::new);
        // SAFETY: garantito dal chiamante.
        unsafe {
            let mut viewport = [0; 4];
            gl.get_parameter_i32_slice(glow::VIEWPORT, &mut viewport);
            let mut color_mask = [0; 4];
            gl.get_parameter_i32_slice(glow::COLOR_WRITEMASK, &mut color_mask);
            let active = gl.get_parameter_i32(glow::ACTIVE_TEXTURE) as u32;
            let mut textures = [None, None];
            for (i, slot) in textures.iter_mut().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                *slot = nz(gl.get_parameter_i32(glow::TEXTURE_BINDING_2D)).map(glow::NativeTexture);
            }
            gl.active_texture(active);
            let flags = [
                glow::BLEND,
                glow::SCISSOR_TEST,
                glow::STENCIL_TEST,
                glow::DEPTH_TEST,
                glow::CULL_FACE,
            ];
            Self {
                framebuffer: current_framebuffer(gl),
                viewport,
                active_texture: active,
                textures,
                program: nz(gl.get_parameter_i32(glow::CURRENT_PROGRAM)).map(glow::NativeProgram),
                vertex_array: nz(gl.get_parameter_i32(glow::VERTEX_ARRAY_BINDING))
                    .map(glow::NativeVertexArray),
                array_buffer: nz(gl.get_parameter_i32(glow::ARRAY_BUFFER_BINDING))
                    .map(glow::NativeBuffer),
                enabled: flags.map(|f| (f, gl.is_enabled(f))),
                color_mask,
                blend_func: [
                    gl.get_parameter_i32(glow::BLEND_SRC_RGB),
                    gl.get_parameter_i32(glow::BLEND_DST_RGB),
                    gl.get_parameter_i32(glow::BLEND_SRC_ALPHA),
                    gl.get_parameter_i32(glow::BLEND_DST_ALPHA),
                ],
                blend_equation: [
                    gl.get_parameter_i32(glow::BLEND_EQUATION_RGB),
                    gl.get_parameter_i32(glow::BLEND_EQUATION_ALPHA),
                ],
                unpack_alignment: gl.get_parameter_i32(glow::UNPACK_ALIGNMENT),
                stencil_mask: gl.get_parameter_i32(glow::STENCIL_WRITEMASK),
                stencil_func: [
                    gl.get_parameter_i32(glow::STENCIL_FUNC),
                    gl.get_parameter_i32(glow::STENCIL_REF),
                    gl.get_parameter_i32(glow::STENCIL_VALUE_MASK),
                ],
                stencil_ops: [
                    gl.get_parameter_i32(glow::STENCIL_FAIL),
                    gl.get_parameter_i32(glow::STENCIL_PASS_DEPTH_FAIL),
                    gl.get_parameter_i32(glow::STENCIL_PASS_DEPTH_PASS),
                ],
                depth_mask: gl.get_parameter_i32(glow::DEPTH_WRITEMASK) != 0,
                depth_func: gl.get_parameter_i32(glow::DEPTH_FUNC),
                scissor: {
                    let mut b = [0; 4];
                    gl.get_parameter_i32_slice(glow::SCISSOR_BOX, &mut b);
                    b
                },
                clear_color: {
                    let mut c = [0.0; 4];
                    gl.get_parameter_f32_slice(glow::COLOR_CLEAR_VALUE, &mut c);
                    c
                },
            }
        }
    }

    /// # Safety
    /// Il contesto di `gl` deve essere corrente.
    unsafe fn restore(self, gl: &glow::Context) {
        // SAFETY: garantito dal chiamante.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, self.framebuffer);
            let [x, y, w, h] = self.viewport;
            gl.viewport(x, y, w, h);
            for (i, texture) in self.textures.iter().enumerate() {
                gl.active_texture(glow::TEXTURE0 + i as u32);
                gl.bind_texture(glow::TEXTURE_2D, *texture);
            }
            gl.active_texture(self.active_texture);
            gl.use_program(self.program);
            gl.bind_vertex_array(self.vertex_array);
            gl.bind_buffer(glow::ARRAY_BUFFER, self.array_buffer);
            for (flag, on) in self.enabled {
                if on {
                    gl.enable(flag);
                } else {
                    gl.disable(flag);
                }
            }
            let m = self.color_mask;
            gl.color_mask(m[0] != 0, m[1] != 0, m[2] != 0, m[3] != 0);
            let f = self.blend_func;
            gl.blend_func_separate(f[0] as u32, f[1] as u32, f[2] as u32, f[3] as u32);
            let e = self.blend_equation;
            gl.blend_equation_separate(e[0] as u32, e[1] as u32);
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, self.unpack_alignment);
            gl.stencil_mask(self.stencil_mask as u32);
            let [func, reference, mask] = self.stencil_func;
            gl.stencil_func(func as u32, reference, mask as u32);
            let [fail, zfail, zpass] = self.stencil_ops;
            gl.stencil_op(fail as u32, zfail as u32, zpass as u32);
            gl.depth_mask(self.depth_mask);
            gl.depth_func(self.depth_func as u32);
            let [sx, sy, sw, sh] = self.scissor;
            gl.scissor(sx, sy, sw, sh);
            let [r, g, b, a] = self.clear_color;
            gl.clear_color(r, g, b, a);
        }
    }
}
