use crate::{
    App, AppState, Message, MessageReceiver, MessageSender, Modal,
    platform::{self, Platform},
    render::{RenderTask, RenderWorkerPool},
    state, wgpu_setup,
};
use log::{error, info, warn};
use wasm_bindgen::prelude::*;

/// Re-export init_thread_pool to be called on the web
#[expect(unused)] // called from javascript only
pub use wasm_bindgen_rayon::init_thread_pool;

use eframe::wasm_bindgen::JsCast;

// YOLO zone
unsafe impl Sync for crate::painters::WgpuResources {}
unsafe impl Send for crate::painters::WgpuResources {}
unsafe impl Send for crate::WgpuError {}
unsafe impl Sync for crate::WgpuError {}

fn get_canvas() -> web_sys::HtmlCanvasElement {
    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");

    document
        .get_element_by_id("the_canvas_id")
        .expect("Failed to find the_canvas_id")
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .expect("the_canvas_id was not a HtmlCanvasElement")
}

fn custom_panic_handler(info: &std::panic::PanicHookInfo) {
    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");
    let p = document.get_element_by_id("panic-message").unwrap();
    p.set_text_content(Some(&format!("{info}")));

    let div = document.get_element_by_id("wasm-panic").unwrap();
    div.remove_attribute("hidden").unwrap();

    get_canvas().remove();
}

#[wasm_bindgen]
pub fn run() {
    let window = web_sys::window().expect("No window");
    let document = window.document().expect("No document");
    let location = window.location();

    let loading = document.get_element_by_id("loading").unwrap();
    loading.remove();

    let params = location
        .search()
        .and_then(|s| web_sys::UrlSearchParams::new_with_str(&s))
        .ok();

    // Get an optional `verbose` parameter from the URL string
    let verbose =
        if let Some(v) = params.as_ref().and_then(|p| p.get("verbose")) {
            match v.as_str() {
                "true" => Ok(true),
                "false" => Ok(false),
                _ => Err(v),
            }
        } else {
            Ok(false)
        };

    // Redirect `log` message to `console.log` and friends:
    eframe::WebLogger::init(if *verbose.as_ref().unwrap_or(&false) {
        // TODO this doesn't seem to work?
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    })
    .ok();

    info!("starting...");
    if let Err(e) = verbose {
        warn!(
            "invalid value for 'verbose': {e:?} (expected 'true' or 'false')"
        );
    }

    let example = params.and_then(|p| p.get("example"));
    const RENDER_POOL_WORKER_COUNT: usize = 4;
    wasm_bindgen_futures::spawn_local(async move {
        // Start the render workers before doing any other work
        start_workers(
            wasm_bindgen::module(),
            wasm_bindgen::memory(),
            wbg_RenderPoolBuilder::new(RENDER_POOL_WORKER_COUNT),
        )
        .await
        .expect("failed to start workers");
        let canvas = get_canvas();
        let mut web_options = eframe::WebOptions::default();
        web_options.wgpu_options.wgpu_setup = match wgpu_setup().await {
            Ok(w) => w.into(),
            Err(e) => {
                let p = document.get_element_by_id("wgpu-error").unwrap();
                p.set_text_content(Some(&format!(
                    "{}",
                    anyhow::Error::from(e),
                )));
                let div = document.get_element_by_id("wgpu-fail").unwrap();
                div.remove_attribute("hidden").unwrap();
                canvas.remove();
                panic!("wgpu initialization failed");
            }
        };

        // Add a custom panic handler for subsequent panics
        std::panic::set_hook(Box::new(custom_panic_handler));

        eframe::WebRunner::new()
            .start(
                canvas,
                web_options,
                Box::new(|cc| {
                    let mut platform = WebPlatform::new(&cc.egui_ctx);
                    let notify_rx = platform.take_notify_rx();
                    let mut app = App::<WebPlatform>::new(cc, platform, false);
                    if let Some(example) = example
                        && !app.load_example(&example)
                    {
                        warn!("failed to load example '{example}'");
                    }

                    // Spawn a worker task to trigger repaints,
                    // per egui#4368 and egui#4405
                    let ctx = cc.egui_ctx.clone();
                    wasm_bindgen_futures::spawn_local(async move {
                        while let Ok(()) = notify_rx.recv_async().await {
                            ctx.request_repaint();
                        }
                        info!("repaint notification task is stopping");
                    });

                    Ok(Box::new(app))
                }),
            )
            .await
            .expect("failed to start eframe");
    });
}

struct WebPlatform {
    /// Dialogs are handled in a separate task
    dialogs: flume::Sender<DialogRequest>,
    rx_channel: Option<MessageReceiver<Notify>>,
    notify_rx: Option<flume::Receiver<()>>,
}

impl WebPlatform {
    fn take_notify_rx(&mut self) -> flume::Receiver<()> {
        self.notify_rx.take().unwrap()
    }
}

impl platform::Platform for WebPlatform {
    type ExportTarget = ExportTarget;
    type Notify = Notify;

    fn new(_ctx: &egui::Context) -> Self {
        let (notify_tx, notify_rx) = flume::unbounded();
        let notify = Notify(notify_tx);
        let (dialog_tx, dialog_rx) = flume::unbounded();
        let rx = MessageReceiver::new(notify);
        wasm_bindgen_futures::spawn_local(dialog_worker(
            dialog_rx,
            rx.sender(),
        ));
        Self {
            dialogs: dialog_tx,
            rx_channel: Some(rx),
            notify_rx: Some(notify_rx),
        }
    }

    fn take_rx_channel(&mut self) -> MessageReceiver<Self::Notify> {
        self.rx_channel.take().unwrap()
    }

    fn update_title(&self, _saved: bool) {
        // no-op on the web backend
    }

    fn can_save(&self) -> bool {
        false
    }

    fn save(&mut self, _state: &AppState) -> std::io::Result<bool> {
        panic!(
            "saving is not supported on web platform, use save_local instead"
        )
    }

    fn save_as(&mut self, _state: &AppState) -> std::io::Result<bool> {
        panic!(
            "saving is not supported on web platform, use save_local instead"
        )
    }

    fn open(&mut self) -> Option<Modal<ExportTarget>> {
        if self.dialogs.send(DialogRequest::Open).is_ok() {
            Some(Modal::WaitForLoad)
        } else {
            error!("could not send Open to dialog thread");
            None
        }
    }

    fn export_name(
        &self,
        name: Option<&str>,
        _dialog_name: &str,
        extension: &str,
    ) -> Option<ExportTarget> {
        if let Some(name) = &name {
            Some(ExportTarget(format!("{name}.{extension}")))
        } else {
            Some(ExportTarget(format!("halfspace_export.{extension}")))
        }
    }

    /// List all "files" in localStorage (keys starting with `vfs:`)
    fn list_local_storage(&self) -> Vec<String> {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .expect("localStorage not available");

        let mut result = Vec::new();
        let len = storage.length().unwrap_or(0);

        for i in 0..len {
            if let Some(key) = storage.key(i).unwrap_or(None)
                && let Some(stripped) = key.strip_prefix(Self::FILE_PREFIX)
            {
                result.push(stripped.to_string());
            }
        }

        result
    }

    /// Write a file (string content) to a given path
    fn save_to_local_storage(&self, path: &str, contents: &str) {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .expect("localStorage not available");

        storage
            .set_item(&format!("{}{path}", Self::FILE_PREFIX), contents)
            .expect("failed to write to localStorage");
    }

    /// Read a file from a given path
    fn read_from_local_storage(&self, path: &str) -> String {
        let storage = web_sys::window()
            .and_then(|w| w.local_storage().ok().flatten())
            .expect("localStorage not available");

        storage
            .get_item(&format!("{}{path}", Self::FILE_PREFIX))
            .unwrap()
            .unwrap()
    }

    fn download_file(
        &self,
        filename: &str,
        data: &[u8],
    ) -> Option<Modal<ExportTarget>> {
        match download_file_inner(filename, data) {
            Ok(()) => None,
            Err(e) => Some(Modal::Error {
                title: "Download failed".to_owned(),
                message: format!("{e:?}"),
            }),
        }
    }

    fn reset(&mut self) {
        // nothing to do here
    }

    fn spawn_render_workers(&mut self) -> RenderWorkerPool<Notify> {
        let tx = TX_CHANNEL.get().unwrap().clone();
        RenderWorkerPool::new(tx)
    }
}

impl WebPlatform {
    /// Prefix to namespace file storage keys
    const FILE_PREFIX: &str = "vfs:";
}

/// Downloads the given file
fn download_file_inner(filename: &str, data: &[u8]) -> Result<(), JsValue> {
    let uint8_array = js_sys::Uint8Array::new_with_length(data.len() as u32);
    uint8_array.copy_from(data);

    let array = js_sys::Array::new();
    array.push(&uint8_array);

    let blob_options = web_sys::BlobPropertyBag::new();
    blob_options.set_type("text/plain");

    // Create and return the Blob
    let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(
        &array,
        &blob_options,
    )?;

    // Create an object URL
    let url = web_sys::Url::create_object_url_with_blob(&blob)?;

    // Save the file
    download_blob(filename, &url)?;

    // Clean up the URL
    web_sys::Url::revoke_object_url(&url)?;

    Ok(())
}

fn download_blob(file_name: &str, url: &str) -> Result<(), JsValue> {
    let document = web_sys::window().unwrap().document().unwrap();

    // Create the anchor element
    let a = document
        .create_element("a")?
        .dyn_into::<web_sys::HtmlAnchorElement>()?;
    a.set_href(url);
    a.set_download(file_name);
    a.set_attribute("style", "display: none")?;

    // Append to body and trigger click
    document.body().unwrap().append_child(&a)?;
    a.click();
    a.remove();

    Ok(())
}

#[derive(Clone)]
pub struct Notify(flume::Sender<()>);

impl platform::Notify for Notify {
    type Err = flume::SendError<()>;
    fn wake(&self) -> Result<(), flume::SendError<()>> {
        self.0.send(())
    }
}

/// Platform-specific export target (downloads to a file)
#[derive(Debug)]
pub struct ExportTarget(String);

impl platform::PlatformExport for ExportTarget {
    fn save(&self, data: &[u8]) -> Result<(), std::io::Error> {
        download_file_inner(&self.0, data)
            .map_err(|e| std::io::Error::other(format!("{e:?}")))
    }
}

pub enum DialogRequest {
    Open,
}

pub(crate) async fn dialog_worker(
    rx: flume::Receiver<DialogRequest>,
    tx: MessageSender<Notify>,
) {
    while let Ok(m) = rx.recv_async().await {
        let r = match m {
            DialogRequest::Open => {
                if let Some(f) = rfd::AsyncFileDialog::new()
                    .add_filter("halfspace", &["half"])
                    .pick_file()
                    .await
                {
                    let data = f.read().await;
                    match std::str::from_utf8(&data)
                        .map_err(state::ReadError::NotUtf8)
                        .and_then(AppState::deserialize)
                    {
                        Ok(state) => Message::Loaded { state },
                        Err(e) => Message::LoadFailed {
                            title: "Open error".to_owned(),
                            message: format!("{:#}", anyhow::Error::from(e)),
                        },
                    }
                } else {
                    Message::CancelLoad
                }
            }
        };
        tx.send(r);
    }
    info!("dialog task is exiting");
}

////////////////////////////////////////////////////////////////////////////////
// Everything below is based on wasm-bindgen-rayon

#[wasm_bindgen(module = "/src/platform/renderWorkers.js")]
extern "C" {
    #[wasm_bindgen(js_name = startWorkers)]
    fn start_workers(
        module: JsValue,
        memory: JsValue,
        builder: wbg_RenderPoolBuilder,
    ) -> js_sys::Promise;
}

#[allow(non_camel_case_types)]
#[wasm_bindgen]
#[doc(hidden)]
struct wbg_RenderPoolBuilder {
    num_threads: usize,
    rx: flume::Receiver<RenderTask<Notify>>,
    sender: flume::Sender<StartTask>,
    receiver: flume::Receiver<StartTask>,
}

pub struct StartTask {
    rx: flume::Receiver<RenderTask<Notify>>,
    ready: flume::Sender<()>,
}

/// Global handle for the tx channel, used to submit tasks to GPU workers
static TX_CHANNEL: std::sync::OnceLock<flume::Sender<RenderTask<Notify>>> =
    std::sync::OnceLock::new();

#[wasm_bindgen]
impl wbg_RenderPoolBuilder {
    fn new(num_threads: usize) -> Self {
        let (sender, receiver) = flume::bounded(num_threads);
        let (tx, rx) = flume::unbounded();
        TX_CHANNEL.set(tx).unwrap();
        Self {
            num_threads,
            sender,
            receiver,
            rx,
        }
    }

    /// Helper to get the main JS file name
    #[wasm_bindgen(js_name = mainJS)]
    pub fn main_js(&self) -> js_sys::JsString {
        #[wasm_bindgen]
        extern "C" {
            #[wasm_bindgen(
                thread_local_v2,
                js_namespace = ["import", "meta"],
                js_name = url
            )]
            static URL: js_sys::JsString;
        }

        URL.with(Clone::clone)
    }

    #[wasm_bindgen(js_name = numThreads)]
    pub fn num_threads(&self) -> usize {
        self.num_threads
    }

    pub fn receiver(&self) -> *const flume::Receiver<StartTask> {
        &self.receiver
    }

    pub async fn build(&mut self) {
        let mut wait_for = Vec::with_capacity(self.num_threads);
        for _i in 0..self.num_threads {
            let (tx, rx) = flume::bounded(0);
            self.sender
                .send(StartTask {
                    rx: self.rx.clone(),
                    ready: tx,
                })
                .unwrap();
            wait_for.push(rx);
        }
        // We have to keep the builder alive until all of the children are done,
        // because they're using its memory (accessed through `receiver`).
        //
        // Crimes, I tell you, crimes!
        for r in wait_for {
            r.recv_async().await.expect("failed to get wait_for rx");
        }
    }
}

#[wasm_bindgen]
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[doc(hidden)]
pub async fn wbg_render_start_worker(
    receiver: *const flume::Receiver<StartTask>,
) where
    flume::Receiver<StartTask>: Sync,
{
    let receiver = unsafe { &*receiver };
    let start = receiver
        .recv_async()
        .await
        .expect("you were supposed to send me a receiver");

    start.ready.send(()).expect("failed to send ready");
    let gpu = crate::render::GpuWorker::new().await;

    // Workaround for https://bugzilla.mozilla.org/show_bug.cgi?id=1870699 :
    // If we are on Firefox, then spam a WebGPU submission at 100 Hz to keep the
    // worker thread polling the GPU.  This is terrible!
    let navigator = js_sys::Reflect::get(
        &js_sys::global(),
        &JsValue::from_str("navigator"),
    )
    .expect("navigator should exist on any global scope");
    let navigator: web_sys::Navigator = navigator.unchecked_into();
    if navigator
        .user_agent()
        .is_ok_and(|ua| ua.contains("Firefox"))
    {
        let device = gpu.gpu.device.clone();
        let queue = gpu.gpu.queue.clone();
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let encoder = device.create_command_encoder(
                    &egui_wgpu::wgpu::CommandEncoderDescriptor {
                        label: Some("heartbeat"),
                    },
                );
                queue.submit(std::iter::once(encoder.finish()));
                sleep(10).await;
            }
        });
    }

    crate::render::render_worker(gpu, start.rx).await;
}

async fn sleep(ms: i32) {
    let global = js_sys::global();
    let scope: web_sys::DedicatedWorkerGlobalScope = global.unchecked_into();
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        scope
            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms)
            .unwrap();
    });
    js_sys::futures::JsFuture::from(promise).await.unwrap();
}
