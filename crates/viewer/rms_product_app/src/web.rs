#![allow(clippy::allow_attributes)] // wasm-bindgen generates attributes that trigger lint noise.

use std::cell::RefCell;
use std::rc::Rc;

use serde::{Deserialize, Serialize};
use wasm_bindgen::{JsCast as _, prelude::*};

use crate::{
    LiveViewerContext, ReplayViewerContext, RmsControlEvent, RmsControlEventSink,
    RmsControlResponse, RmsHostEvent, RmsHostEventSink, RmsProductApp, RmsSourceKind,
    RmsTopicContext, RmsViewerContext,
};

/// `JavaScript` handle for the product-owned RMS `WebAssembly` application.
#[wasm_bindgen]
pub struct RmsWebHandle {
    runner: eframe::WebRunner,
    event_callback: Rc<RefCell<Option<js_sys::Function>>>,
}

fn dispatch_event<T: Serialize>(event_callback: &Rc<RefCell<Option<js_sys::Function>>>, event: &T) {
    let Some(callback) = event_callback.borrow().as_ref().cloned() else {
        return;
    };
    let Ok(value) = serde_wasm_bindgen::to_value(event) else {
        return;
    };
    drop(callback.call1(&JsValue::NULL, &value));
}

#[derive(Clone, Debug, Deserialize)]
struct LegacyViewerContext {
    project_name: String,
    device_id: String,
    device_name: String,
    device_status: String,
    device_health: String,
    device_state_version: u32,
    source_id: String,
    source_name: String,
    source_kind: RmsSourceKind,
    source_url: String,
    operator_id: String,
    #[serde(default)]
    control_enabled: bool,
    topics: Vec<RmsTopicContext>,
}

impl LegacyViewerContext {
    fn into_tagged(self) -> RmsViewerContext {
        match self.source_kind {
            RmsSourceKind::Live => RmsViewerContext::Live(LiveViewerContext {
                project_id: String::new(),
                project_name: self.project_name,
                device_id: self.device_id,
                device_name: self.device_name,
                device_status: self.device_status,
                device_health: self.device_health,
                device_state_version: self.device_state_version,
                live_session_id: self.source_id.clone(),
                data_source_id: self.source_id,
                source_name: self.source_name,
                source_url: self.source_url,
                operator_id: self.operator_id,
                control_enabled: self.control_enabled,
                topics: self.topics,
            }),
            RmsSourceKind::Recording => RmsViewerContext::Replay(ReplayViewerContext {
                project_id: String::new(),
                project_name: self.project_name,
                device_id: self.device_id,
                device_name: self.device_name,
                recording_id: self.source_id.clone(),
                recording_name: self.source_name,
                replay_session_id: self.source_id,
                source_url: self.source_url,
                captured_at_label: None,
                initial_timeline: String::new(),
                initial_fps: None,
                initial_cursor: None,
                initial_play_state: Default::default(),
                initial_speed: 1.0,
                initial_loop: Default::default(),
                topics: self.topics,
            }),
        }
    }
}

impl RmsWebHandle {
    fn host_event_sink(&self) -> RmsHostEventSink {
        let event_callback = self.event_callback.clone();
        Rc::new(move |event: RmsHostEvent| dispatch_event(&event_callback, &event))
    }

    fn control_event_sink(&self) -> RmsControlEventSink {
        let event_callback = self.event_callback.clone();
        Rc::new(move |event: RmsControlEvent| dispatch_event(&event_callback, &event))
    }

    fn has_event_callback(&self) -> bool {
        self.event_callback.borrow().is_some()
    }

    fn running_app(&self) -> Result<std::cell::RefMut<'_, RmsProductApp>, JsValue> {
        self.runner
            .app_mut::<RmsProductApp>()
            .ok_or_else(|| js_sys::Error::new("RMS runtime is not running").into())
    }
}

#[wasm_bindgen]
impl RmsWebHandle {
    /// Creates a stopped RMS runtime.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            runner: eframe::WebRunner::new(),
            event_callback: Default::default(),
        }
    }

    /// Registers the RMS HTTP/SSE transport adapter callback.
    #[wasm_bindgen]
    pub fn set_event_callback(&self, callback: Option<js_sys::Function>) {
        if callback.is_none()
            && let Some(mut app) = self.runner.app_mut::<RmsProductApp>()
        {
            // Revoke while the previous callback is still available to release any active lease.
            app.revoke_control_capability();
        }
        *self.event_callback.borrow_mut() = callback;
    }

    /// Starts the RMS application in the supplied canvas.
    #[wasm_bindgen]
    pub async fn start(&self, canvas: JsValue) -> Result<(), JsValue> {
        let canvas = canvas
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|value| format!("Expected a canvas element, got {value:?}"))?;
        let main_thread_token = re_viewer::MainThreadToken::i_promise_i_am_on_the_main_thread();
        let host_event_sink = self.host_event_sink();

        let web_options = eframe::WebOptions {
            wgpu_options: re_viewer::wgpu_options(None),
            depth_buffer: 0,
            dithering: true,
            ..Default::default()
        };

        self.runner
            .start(
                canvas,
                web_options,
                Box::new(move |creation_context| {
                    Ok(Box::new(RmsProductApp::new(
                        main_thread_token,
                        creation_context,
                        None,
                        Some(host_event_sink),
                    )?))
                }),
            )
            .await?;

        Ok(())
    }

    /// Applies the authenticated context returned by the RMS Live service.
    ///
    /// Calling this method injects the control-event capability for this Live session.
    #[wasm_bindgen]
    pub fn apply_live_context(&self, context: JsValue) -> Result<(), JsValue> {
        let context: LiveViewerContext = serde_wasm_bindgen::from_value(context)?;
        let control_event_sink = (context.control_enabled && self.has_event_callback())
            .then(|| self.control_event_sink());
        let mut app = self.running_app()?;
        app.apply_live_context(context, control_event_sink);
        Ok(())
    }

    /// Applies the authenticated context returned by the RMS Replay service.
    ///
    /// No control-event capability is created or passed to the Replay runtime.
    #[wasm_bindgen]
    pub fn apply_replay_context(&self, context: JsValue) -> Result<(), JsValue> {
        let context: ReplayViewerContext = serde_wasm_bindgen::from_value(context)?;
        let mut app = self.running_app()?;
        app.apply_replay_context(context);
        Ok(())
    }

    /// Compatibility bridge for the former combined context contract.
    ///
    /// New hosts must call [`Self::apply_live_context`] or [`Self::apply_replay_context`].
    #[wasm_bindgen]
    pub fn apply_viewer_context(&self, context: JsValue) -> Result<(), JsValue> {
        let tagged =
            serde_wasm_bindgen::from_value::<RmsViewerContext>(context.clone()).or_else(|_| {
                serde_wasm_bindgen::from_value::<LegacyViewerContext>(context)
                    .map(LegacyViewerContext::into_tagged)
            })?;
        let control_event_sink = matches!(&tagged, RmsViewerContext::Live(context)
            if context.control_enabled && self.has_event_callback())
        .then(|| self.control_event_sink());
        let mut app = self.running_app()?;
        app.apply_viewer_context(tagged, control_event_sink);
        Ok(())
    }

    /// Applies a control response returned by the RMS Live service adapter.
    #[wasm_bindgen]
    pub fn apply_control_response(&self, response: JsValue) -> Result<(), JsValue> {
        let response: RmsControlResponse = serde_wasm_bindgen::from_value(response)?;
        let mut app = self.running_app()?;
        app.apply_control_response(response);
        Ok(())
    }

    /// Reports a session resolution failure without exposing technical details.
    #[wasm_bindgen]
    pub fn apply_viewer_error(&self, message: String) -> Result<(), JsValue> {
        let mut app = self.running_app()?;
        app.apply_viewer_error(message);
        Ok(())
    }

    /// Starts fail-closed control cleanup before the host destroys the runtime.
    ///
    /// The host must continue applying control responses and call this method again until it
    /// returns `true`, then call [`Self::stop`].
    #[wasm_bindgen]
    pub fn prepare_stop(&self) -> Result<bool, JsValue> {
        let mut app = self.running_app()?;
        Ok(app.prepare_for_shutdown())
    }

    /// Stops the `WebAssembly` application and releases its graphics resources.
    #[wasm_bindgen]
    pub fn stop(&self) {
        if let Some(mut app) = self.runner.app_mut::<RmsProductApp>() {
            // Best effort for legacy hosts. New hosts wait for `prepare_stop` to return `true`.
            app.prepare_for_shutdown();
        }
        self.runner.destroy();
        self.event_callback.borrow_mut().take();
    }
}

impl Default for RmsWebHandle {
    fn default() -> Self {
        Self::new()
    }
}
