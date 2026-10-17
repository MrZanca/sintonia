//! Disegno del video di mpv in un contesto OpenGL esistente (render API di libmpv).

use std::cell::Cell;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::fmt;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::Arc;

use crate::{check, ffi, Core, Player, PlayerError};

/// Funzione che risolve i simboli OpenGL del contesto corrente (come `wglGetProcAddress`).
pub type GetProcAddress<'a> = &'a dyn Fn(&CStr) -> *const c_void;

/// Funzione chiamata da un thread di mpv quando c'è un nuovo fotogramma da disegnare.
type UpdateCallback = Box<dyn Fn() + Send + Sync>;

/// Contesto di disegno di mpv legato a **un** contesto OpenGL.
///
/// mpv ne ammette uno solo per core. Non è `Send`: si usa solo dal thread che possiede il
/// contesto OpenGL (in Sintonia, quello dell'interfaccia).
pub struct RenderContext {
    ctx: NonNull<ffi::mpv_render_context>,
    /// Puntatore alla funzione di risoluzione valida *durante* la chiamata in corso, o NULL.
    /// mpv la usa nella creazione e, per alcune estensioni, anche dopo: per questo la
    /// impostiamo a ogni chiamata invece di tenerne una copia che potrebbe scadere.
    loader: Box<Cell<*const GetProcAddress<'static>>>,
    /// Letta solo da mpv attraverso il puntatore registrato; deve vivere quanto `ctx`.
    _update: Box<UpdateCallback>,
    _core: Arc<Core>,
}

impl fmt::Debug for RenderContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderContext").finish_non_exhaustive()
    }
}

/// Chiamata da mpv per risolvere una funzione OpenGL.
unsafe extern "C" fn get_proc_address_trampoline(
    ctx: *mut c_void,
    name: *const c_char,
) -> *mut c_void {
    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: `ctx` è il `Cell` dentro `RenderContext::loader`, che vive quanto il contesto.
        let slot = unsafe { &*(ctx as *const Cell<*const GetProcAddress<'static>>) };
        let loader = slot.get();
        if loader.is_null() || name.is_null() {
            return ptr::null_mut();
        }
        // SAFETY: `loader` non è NULL solo dentro `with_loader`, dove punta a un riferimento vivo
        // sullo stack del chiamante; `name` è una stringa C di mpv.
        unsafe { (*loader)(CStr::from_ptr(name)) as *mut c_void }
    }));
    result.unwrap_or(ptr::null_mut())
}

/// Chiamata da un thread di mpv quando serve ridisegnare.
unsafe extern "C" fn update_trampoline(ctx: *mut c_void) {
    // SAFETY: `ctx` è il `Box<UpdateCallback>` di `RenderContext::update`, liberato solo dopo
    // `mpv_render_context_free` (che garantisce che la callback non venga più chiamata).
    let callback = unsafe { &*(ctx as *const UpdateCallback) };
    if catch_unwind(AssertUnwindSafe(callback)).is_err() {
        log::error!("panic nella callback di aggiornamento del video");
    }
}

impl RenderContext {
    /// Collega mpv al contesto OpenGL **corrente**.
    ///
    /// `on_update` viene chiamata da un thread di mpv quando c'è un nuovo fotogramma: non deve
    /// chiamare funzioni di mpv, solo chiedere un ridisegno (per esempio con
    /// `slint::invoke_from_event_loop`).
    ///
    /// # Safety
    /// - Il contesto OpenGL per cui `get_proc_address` risolve i simboli deve essere corrente sul
    ///   thread chiamante adesso, a ogni chiamata dei metodi di questo oggetto **e quando viene
    ///   distrutto** (`Drop` chiama `mpv_render_context_free`, che usa OpenGL).
    /// - Non deve esistere un altro `RenderContext` per lo stesso `player`.
    pub unsafe fn new(
        player: &Player,
        get_proc_address: GetProcAddress<'_>,
        on_update: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, PlayerError> {
        let core = Arc::clone(player.core());
        let loader: Box<Cell<*const GetProcAddress<'static>>> = Box::new(Cell::new(ptr::null()));
        let update: Box<UpdateCallback> = Box::new(Box::new(on_update));

        let mut init = ffi::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address_trampoline),
            get_proc_address_ctx: (&*loader as *const Cell<_>).cast_mut().cast(),
        };
        let mut params = [
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_API_TYPE,
                data: ffi::MPV_RENDER_API_TYPE_OPENGL.as_ptr().cast_mut().cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: (&mut init as *mut ffi::mpv_opengl_init_params).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        let mut raw = ptr::null_mut();
        let code = with_loader(&loader, get_proc_address, || {
            // SAFETY: core valido; `params` è terminato da INVALID e i dati a cui punta vivono
            // fino al ritorno; il contesto GL è corrente (contratto di `new`).
            unsafe { ffi::mpv_render_context_create(&mut raw, core.as_ptr(), params.as_mut_ptr()) }
        });
        check("creazione del contesto video", code)?;
        let ctx = NonNull::new(raw).ok_or(PlayerError::Mpv {
            what: "creazione del contesto video",
            code: 0,
            message: "mpv ha restituito un contesto nullo".into(),
        })?;

        // SAFETY: contesto appena creato; `update` vive finché il contesto non è liberato (Drop).
        unsafe {
            ffi::mpv_render_context_set_update_callback(
                ctx.as_ptr(),
                Some(update_trampoline),
                (&*update as *const UpdateCallback).cast_mut().cast(),
            );
        }

        Ok(Self {
            ctx,
            loader,
            _update: update,
            _core: core,
        })
    }

    /// `true` se mpv ha un nuovo fotogramma da disegnare. Va chiamata dopo ogni `on_update`.
    pub fn frame_ready(&self) -> bool {
        // SAFETY: contesto valido; GL corrente per contratto di `new`.
        let flags = unsafe { ffi::mpv_render_context_update(self.ctx.as_ptr()) };
        flags & ffi::MPV_RENDER_UPDATE_FRAME != 0
    }

    /// Disegna il fotogramma corrente nel framebuffer `fbo` (`0` = quello della finestra),
    /// grande `width` × `height` pixel, adattandolo e aggiungendo le bande nere se serve.
    ///
    /// Con `flip_y = false` la prima riga in memoria è quella in alto (adatto alle texture).
    pub fn render(
        &self,
        get_proc_address: GetProcAddress<'_>,
        fbo: u32,
        width: i32,
        height: i32,
        flip_y: bool,
    ) -> Result<(), PlayerError> {
        let mut target = ffi::mpv_opengl_fbo {
            fbo: c_int::try_from(fbo).unwrap_or(0),
            w: width,
            h: height,
            internal_format: 0,
        };
        let mut flip: c_int = flip_y.into();
        let mut params = [
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_OPENGL_FBO,
                data: (&mut target as *mut ffi::mpv_opengl_fbo).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_FLIP_Y,
                data: (&mut flip as *mut c_int).cast(),
            },
            ffi::mpv_render_param {
                type_: ffi::MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let code = with_loader(&self.loader, get_proc_address, || {
            // SAFETY: contesto valido, GL corrente (contratto di `new`), `params` terminato da
            // INVALID con dati vivi fino al ritorno.
            unsafe { ffi::mpv_render_context_render(self.ctx.as_ptr(), params.as_mut_ptr()) }
        });
        check("disegno del video", code)
    }

    /// Da chiamare dopo aver presentato il fotogramma (aiuta mpv a regolare i tempi).
    pub fn report_swap(&self) {
        // SAFETY: contesto valido; GL corrente per contratto di `new`.
        unsafe { ffi::mpv_render_context_report_swap(self.ctx.as_ptr()) };
    }
}

impl Drop for RenderContext {
    fn drop(&mut self) {
        // SAFETY: contesto valido e GL corrente (contratto di `new`). Dopo questa chiamata mpv
        // non usa più né la callback di aggiornamento né la funzione di risoluzione, che quindi
        // possono essere liberate (lo fa il drop dei campi, dopo).
        unsafe { ffi::mpv_render_context_free(self.ctx.as_ptr()) };
    }
}

/// Rende disponibile `get_proc_address` a mpv solo per la durata di `f`.
fn with_loader<R>(
    slot: &Cell<*const GetProcAddress<'static>>,
    get_proc_address: GetProcAddress<'_>,
    f: impl FnOnce() -> R,
) -> R {
    struct Reset<'a>(&'a Cell<*const GetProcAddress<'static>>);
    impl Drop for Reset<'_> {
        fn drop(&mut self) {
            self.0.set(ptr::null());
        }
    }
    // Il riferimento `get_proc_address` vive sullo stack fino alla fine di questa funzione; lo
    // slot viene azzerato prima del ritorno (anche con un panic), quindi il puntatore con la
    // durata "allungata" a 'static non viene mai letto dopo che il riferimento è scaduto.
    let as_static: *const GetProcAddress<'static> =
        (&get_proc_address as *const GetProcAddress<'_>).cast();
    slot.set(as_static);
    let _reset = Reset(slot);
    f()
}
