#![allow(clippy::allow_attributes)] // wasm-bindgen generates attributes that trigger lint noise.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::{JsCast as _, prelude::*};

use crate::{RmsControlResponse, RmsProductApp, RmsProductEventSink, RmsViewerContext};

/// `JavaScript` handle for the product-owned RMS `WebAssembly` application.
#[wasm_bindgen]
pub struct RmsWebHandle {
    runner: eframe::WebRunner,
    event_callback: Rc<RefCell<Option<js_sys::Function>>>,
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
        *self.event_callback.borrow_mut() = callback;
    }

    /// Starts the RMS application in the supplied canvas.
    #[wasm_bindgen]
    pub async fn start(&self, canvas: JsValue) -> Result<(), JsValue> {
        let canvas = canvas
            .dyn_into::<web_sys::HtmlCanvasElement>()
            .map_err(|value| format!("Expected a canvas element, got {value:?}"))?;
        let main_thread_token = re_viewer::MainThreadToken::i_promise_i_am_on_the_main_thread();
        let event_callback = self.event_callback.clone();
        let event_sink: RmsProductEventSink = Rc::new(move |event| {
            let Some(callback) = event_callback.borrow().as_ref().cloned() else {
                return;
            };
            let Ok(value) = serde_wasm_bindgen::to_value(&event) else {
                return;
            };
            drop(callback.call1(&JsValue::NULL, &value));
        });

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
                        Some(event_sink),
                    )?))
                }),
            )
            .await?;

        Ok(())
    }

    /// Applies the authenticated workspace context returned by the RMS backend adapter.
    #[wasm_bindgen]
    pub fn apply_viewer_context(&self, context: JsValue) -> Result<(), JsValue> {
        let context: RmsViewerContext = serde_wasm_bindgen::from_value(context)?;
        let Some(mut app) = self.runner.app_mut::<RmsProductApp>() else {
            return Err(js_sys::Error::new("RMS runtime is not running").into());
        };
        app.apply_viewer_context(context);
        Ok(())
    }

    /// Applies a control response returned by the RMS backend adapter.
    #[wasm_bindgen]
    pub fn apply_control_response(&self, response: JsValue) -> Result<(), JsValue> {
        let response: RmsControlResponse = serde_wasm_bindgen::from_value(response)?;
        let Some(mut app) = self.runner.app_mut::<RmsProductApp>() else {
            return Err(js_sys::Error::new("RMS runtime is not running").into());
        };
        app.apply_control_response(response);
        Ok(())
    }

    /// Reports a workspace or source resolution failure without exposing technical details.
    #[wasm_bindgen]
    pub fn apply_viewer_error(&self, message: String) -> Result<(), JsValue> {
        let Some(mut app) = self.runner.app_mut::<RmsProductApp>() else {
            return Err(js_sys::Error::new("RMS runtime is not running").into());
        };
        app.apply_viewer_error(message);
        Ok(())
    }

    /// Stops the `WebAssembly` application and releases its graphics resources.
    #[wasm_bindgen]
    pub fn stop(&self) {
        self.event_callback.borrow_mut().take();
        self.runner.destroy();
    }
}

impl Default for RmsWebHandle {
    fn default() -> Self {
        Self::new()
    }
}
