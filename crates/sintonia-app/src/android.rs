//! Android: punto d'ingresso, ciclo di vita e i pezzi che sul PC fa il sistema (selettore di
//! file, browser, schermo acceso, messaggi).
//!
//! L'APK carica `libsintonia_app.so` dentro `SintoniaActivity` (una `NativeActivity` con poche
//! righe di Java, `android/java/...`), e `android-activity` chiama [`android_main`]. Le chiamate
//! verso Java passano dalla JVM con JNI; l'unica in senso inverso è il file scelto nel selettore.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use android_activity::{AndroidApp, MainEvent, PollEvent};
use jni::objects::{JClass, JObject, JString, JValue};
use jni::{jni_sig, jni_str, Env, EnvUnowned, JavaVM, NativeMethod, Outcome};
use sintonia_player::lifecycle::{Lifecycle, LifecycleEvent, Transition};
use slint::platform::WindowEvent;
use slint::{ComponentHandle, LogicalSize};

use crate::video::VideoRouter;
use crate::MainWindow;

static APP: OnceLock<AndroidApp> = OnceLock::new();

/// Il callback del selettore di file in attesa (thread dell'interfaccia).
type Picked = Box<dyn FnOnce(Option<PathBuf>)>;

thread_local! {
    /// Ciclo di vita e il video da sospendere; i segnali arrivano sul thread dell'interfaccia.
    static LIFECYCLE: RefCell<(Lifecycle, Option<Rc<VideoRouter>>)> = RefCell::default();
    static PICKED: RefCell<Option<Picked>> = const { RefCell::new(None) };
    static FIT: slint::Timer = slint::Timer::default();
}

/// La grandezza per cui sono disegnate le pagine (punti logici), come `zoom.rs` sul PC.
const DESIGN_WIDTH: f32 = 1320.0;
const DESIGN_HEIGHT: f32 = 800.0;

/// La scala che fa stare intero il disegno da 1320 × 800 in `width` × `height` pixel.
fn fit_scale(width: u32, height: u32) -> Option<f32> {
    let scale = (width as f32 / DESIGN_WIDTH).min(height as f32 / DESIGN_HEIGHT);
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

/// Fase 3: la stessa interfaccia del PC, rimpicciolita o ingrandita perché stia intera sullo
/// schermo (con la scala di Android, 2,75 sul Mi 9T, le pagine non ci starebbero). Si ricontrolla
/// ogni mezzo secondo: rotazione e cambi di configurazione rimettono la scala di Android.
/// L'interfaccia pensata per il telefono è la fase 4.
pub fn fit_design(main: &MainWindow) {
    let weak = main.as_weak();
    FIT.with(|timer| {
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(500),
            move || {
                let Some(main) = weak.upgrade() else { return };
                let window = main.window();
                let size = window.size();
                let Some(scale) = fit_scale(size.width, size.height) else {
                    return;
                };
                if (window.scale_factor() - scale).abs() < 0.01 {
                    return;
                }
                log::info!(
                    "interfaccia in scala {scale:.2} per {}×{} pixel",
                    size.width,
                    size.height
                );
                window.dispatch_event(WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                });
                window.dispatch_event(WindowEvent::Resized {
                    size: LogicalSize::new(size.width as f32 / scale, size.height as f32 / scale),
                });
            },
        );
    });
}

#[no_mangle]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("sintonia"),
    );
    std::panic::set_hook(Box::new(|info| log::error!("PANIC: {info}")));
    log::info!("Sintonia {} per Android", env!("CARGO_PKG_VERSION"));

    // SAFETY: i puntatori vengono da `AndroidApp`, che tiene vive la JavaVM e l'activity finché
    // `android_main` non ritorna (e il core li usa solo da lì in poi).
    if let Err(err) =
        unsafe { sintonia_core::android::init(app.vm_as_ptr(), app.activity_as_ptr()) }
    {
        log::error!("Android: {err}");
        return;
    }
    // SAFETY: `vm_as_ptr` è la JavaVM del processo, valida per tutta la sua vita.
    if let Err(err) = unsafe { sintonia_player::set_java_vm(app.vm_as_ptr()) } {
        log::error!("{err}");
    }
    if let Err(err) = with_activity(&app, register_natives) {
        // senza, il selettore di file non risponde: il resto dell'app funziona
        log::error!("metodi nativi dell'activity: {err}");
    }
    let _ = APP.set(app.clone());
    if let Err(err) = slint::android::init_with_event_listener(app, on_event) {
        log::error!("backend Android di Slint: {err}");
        return;
    }
    if let Err(err) = crate::run() {
        log::error!("{err}");
    }
}

/// Collega il video al ciclo di vita dell'activity.
pub fn set_router(router: Rc<VideoRouter>) {
    LIFECYCLE.with(|l| l.borrow_mut().1 = Some(router));
}

/// Il video va in secondo piano con l'app: Android distrugge la superficie e mpv non deve
/// continuare a decodificare per niente (vedi `sintonia_player::lifecycle`).
fn on_event(event: &PollEvent<'_>) {
    let event = match event {
        PollEvent::Main(MainEvent::Start) => LifecycleEvent::Started,
        PollEvent::Main(MainEvent::Stop) => LifecycleEvent::Stopped,
        PollEvent::Main(MainEvent::InitWindow { .. }) => LifecycleEvent::WindowCreated,
        PollEvent::Main(MainEvent::TerminateWindow { .. }) => LifecycleEvent::WindowDestroyed,
        _ => return,
    };
    let (transition, router) = LIFECYCLE.with(|l| {
        let mut l = l.borrow_mut();
        (l.0.handle(event), l.1.clone())
    });
    log::info!("ciclo di vita: {event:?} -> {transition:?}");
    match (transition, router) {
        (Some(Transition::Suspend), Some(router)) => router.suspend(),
        (Some(Transition::Resume), Some(router)) => router.resume(),
        _ => {}
    }
}

/// Esegue `f` con la JVM e l'activity, da qualunque thread.
fn with_activity<T>(
    app: &AndroidApp,
    f: impl FnOnce(&mut Env<'_>, &JObject<'_>) -> jni::errors::Result<T>,
) -> jni::errors::Result<T> {
    // SAFETY: `vm_as_ptr` è la JavaVM del processo, valida finché il processo vive.
    let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        // SAFETY: `activity_as_ptr` è un riferimento globale all'activity, valido finché vive
        // `AndroidApp`; `JObject` non lo libera mai, lo si usa solo per chiamarci sopra metodi.
        let activity = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        f(env, &activity)
    })
}

/// Chiama un metodo `void` dell'activity con argomenti stringa.
fn call_activity(
    method: &'static jni::strings::JNIStr,
    sig: jni::signature::MethodSignature<'static, 'static>,
    args: &[&str],
) {
    let Some(app) = APP.get() else { return };
    let result = with_activity(app, |env, activity| {
        let strings = args
            .iter()
            .map(|a| env.new_string(a))
            .collect::<Result<Vec<_>, _>>()?;
        let values: Vec<JValue> = strings.iter().map(|s| JValue::Object(s)).collect();
        env.call_method(activity, method, sig, &values)?;
        Ok(())
    });
    if let Err(err) = result {
        log::error!("activity, {method}: {err}");
    }
}

fn register_natives(env: &mut Env<'_>, activity: &JObject<'_>) -> jni::errors::Result<()> {
    let class = env.get_object_class(activity)?;
    // SAFETY: `on_file_picked` ha la firma di un metodo statico `(Ljava/lang/String;)V`:
    // ambiente JNI, classe e una stringa, nessun valore di ritorno.
    let method = unsafe {
        NativeMethod::from_raw_parts(
            jni_str!("onFilePicked"),
            jni_str!("(Ljava/lang/String;)V"),
            on_file_picked as *mut std::ffi::c_void,
        )
    };
    // SAFETY: il metodo registrato corrisponde alla dichiarazione `static native` di Java.
    unsafe { env.register_native_methods(&class, &[method]) }
}

/// Chiamato da Java (su un suo thread) con il file scelto, già copiato nella cartella dei dati.
extern "system" fn on_file_picked<'local>(
    mut env: EnvUnowned<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) {
    let path = match env
        .with_env(|env| -> jni::errors::Result<Option<String>> {
            if path.is_null() {
                return Ok(None);
            }
            Ok(Some(path.try_to_string(env)?))
        })
        .into_outcome()
    {
        Outcome::Ok(path) => path.map(PathBuf::from),
        Outcome::Err(err) => {
            log::error!("file scelto: {err}");
            None
        }
        Outcome::Panic(_) => None,
    };
    let delivered = slint::invoke_from_event_loop(move || {
        if let Some(callback) = PICKED.with(|p| p.borrow_mut().take()) {
            callback(path);
        }
    });
    if let Err(err) = delivered {
        log::error!("file scelto: {err}");
    }
}

/// Apre il selettore di file di Android; `on_picked` riceve il percorso della copia (`None`
/// se annullato). Da chiamare dal thread dell'interfaccia.
pub fn pick_file(on_picked: impl FnOnce(Option<PathBuf>) + 'static) {
    PICKED.with(|p| *p.borrow_mut() = Some(Box::new(on_picked)));
    call_activity(jni_str!("pickPlaylist"), jni_sig!("()V"), &[]);
}

/// Apre una pagina nel browser.
pub fn open_url(url: &str) {
    call_activity(
        jni_str!("openUrl"),
        jni_sig!("(Ljava/lang/String;)V"),
        &[url],
    );
}

/// Un messaggio con il pulsante OK.
pub fn show_message(title: &str, text: &str) {
    call_activity(
        jni_str!("showMessage"),
        jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
        &[title, text],
    );
}

/// Schermo sempre acceso (`FLAG_KEEP_SCREEN_ON`) finché si guarda qualcosa. Passa dall'activity
/// Java: `AndroidApp::set_window_flags` chiamato prima che la finestra esista (ripresa del canale
/// all'avvio) bloccava il thread dell'app.
pub fn keep_screen_on(on: bool) {
    let Some(app) = APP.get() else { return };
    let result = with_activity(app, |env, activity| {
        env.call_method(
            activity,
            jni_str!("keepScreenOn"),
            jni_sig!("(Z)V"),
            &[JValue::Bool(on)],
        )?;
        Ok(())
    });
    if let Err(err) = result {
        log::error!("activity, keepScreenOn: {err}");
    }
}
