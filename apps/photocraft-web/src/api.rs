//! `window.photocraft`: the page command API (`craft-web-api`) over the app's own control channel,
//! the one the desktop control server feeds (`docs/control-protocol.md`).
//!
//! The crate owns the JavaScript side (`request`, `execute`, `commands`, `docInspect`, `uiInspect`,
//! `screenshot`, as promises with a timeout). This module is PhotoCraft's part: each call queues a
//! [`ControlRequest`] that the app answers on its next frame (`PhotocraftApp::with_control`), and
//! waits for the reply. A call the page gave up on is released ([`Pending`]): the app skips it if
//! it has not started it (`Dispatch::give_up`), instead of applying a command the page was told
//! had failed.
//!
//! It is installed only when the page URL has `?control` (as PdfCraft does): a page that does not
//! ask for it gets no global.

use std::sync::mpsc::{Receiver, TryRecvError};

#[cfg(test)]
use photocraft_ui_egui::ControlRequest;
use photocraft_ui_egui::control::Dispatch;
use serde_json::{Value, json};

/// How often a waiting call looks for the app's reply, in milliseconds (about a frame).
#[cfg(target_arch = "wasm32")]
const POLL_MS: i32 = 16;

/// Whether the page asked for `window.photocraft`: a `control` key in the query (`?control`,
/// `?control=1`, `?webgl&control`), with or without the leading `?`.
pub fn wanted(query: &str) -> bool {
    let query = query.strip_prefix('?').unwrap_or(query);
    query.split('&').any(|pair| pair.split('=').next() == Some("control"))
}

/// The control method and params PhotoCraft's channel takes for a call from the page.
///
/// - `docInspect` asks for `document.inspect`, which PhotoCraft has as an engine command rather
///   than a control method: it goes through `engine.execute`.
/// - `screenshot` adds `data: true`; PhotoCraft's `ui.screenshot` rejects fields it doesn't know
///   and already returns the PNG (`base64`) when no `path` is given, so `data` is dropped.
pub fn adapt(method: String, params: Value) -> (String, Value) {
    match method.as_str() {
        "document.inspect" => {
            let params = if params.is_null() { json!({}) } else { params };
            ("engine.execute".to_string(), json!({"command": "document.inspect", "params": params}))
        }
        "ui.screenshot" => {
            let mut params = params;
            if let Some(o) = params.as_object_mut() {
                o.remove("data");
            }
            (method, params)
        }
        _ => (method, params),
    }
}

/// A queued control request whose reply has not arrived. Dropping it before the reply (the page's
/// timeout fired, or the call was abandoned) releases the request: the app skips it if it has not
/// started it (one it has started still finishes).
struct Pending {
    reply: Receiver<Value>,
    dispatch: Dispatch,
    answered: bool,
}

impl Pending {
    fn new(reply: Receiver<Value>, dispatch: Dispatch) -> Self {
        Self { reply, dispatch, answered: false }
    }

    /// `Some` once the app has answered or gone away; `None` while the request is still waiting.
    fn poll(&mut self) -> Option<Value> {
        match self.reply.try_recv() {
            Ok(reply) => {
                self.answered = true;
                Some(reply)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.answered = true;
                Some(json!({"ok": false, "error": "the app dropped the request"}))
            }
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if !self.answered {
            // The reply text is for a transport that answers someone; the page has gone already.
            let _ = self.dispatch.give_up();
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::install;

#[cfg(target_arch = "wasm32")]
mod web {
    use std::rc::Rc;
    use std::sync::mpsc::channel;

    use craft_web_api::methods::Methods;
    use craft_web_api::page::{PageApi, Request};
    use photocraft_ui_egui::{ControlRequest, PhotocraftApp};
    use serde_json::json;
    use wasm_bindgen_futures::JsFuture;

    use super::{POLL_MS, Pending, adapt};

    /// The app id in `window.<app>`.
    const APP: &str = "photocraft";

    /// Give the app its control queue and put `window.photocraft` on the page. An existing
    /// `window.photocraft` keeps the functions it already has.
    pub fn install(app: PhotocraftApp, ctx: egui::Context) -> PhotocraftApp {
        let (queue, requests) = channel::<ControlRequest>();
        let request: Request = Rc::new(move |method, params| {
            let (method, params) = adapt(method, params);
            let (control, reply) = ControlRequest::new(method, params);
            let mut pending = Pending::new(reply, control.dispatch.clone());
            if queue.send(control).is_err() {
                pending.answered = true;
                return Box::pin(async { json!({"ok": false, "error": "the app is not running"}) });
            }
            ctx.request_repaint();
            Box::pin(async move {
                loop {
                    if let Some(reply) = pending.poll() {
                        return reply;
                    }
                    sleep(POLL_MS).await;
                }
            })
        });
        if let Err(e) = Rc::new(PageApi::new(APP, Methods::default(), request)).install() {
            log::warn!("couldn't install window.{APP}: {e:?}");
        }
        app.with_control(requests)
    }

    async fn sleep(ms: i32) {
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
            }
        });
        let _ = JsFuture::from(promise).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending() -> (Pending, ControlRequest) {
        let (control, reply) = ControlRequest::new("ui.inspect", json!({}));
        (Pending::new(reply, control.dispatch.clone()), control)
    }

    #[test]
    fn the_api_is_installed_only_when_the_page_asks_for_it() {
        for query in ["?control", "control", "?control=1", "?webgl&control", "?control&host=parent"] {
            assert!(wanted(query), "{query}");
        }
        for query in ["", "?", "?webgl", "?controls", "?nocontrol", "?x=control"] {
            assert!(!wanted(query), "{query}");
        }
    }

    #[test]
    fn doc_inspect_goes_through_the_engine_command() {
        let (method, params) = adapt("document.inspect".into(), json!({"layers": true}));
        assert_eq!(method, "engine.execute");
        assert_eq!(params, json!({"command": "document.inspect", "params": {"layers": true}}));
        let (_, params) = adapt("document.inspect".into(), Value::Null);
        assert_eq!(params["params"], json!({}));
    }

    #[test]
    fn screenshot_drops_the_data_flag_photocraft_does_not_take() {
        let (method, params) = adapt("ui.screenshot".into(), json!({"data": true, "focus": false}));
        assert_eq!(method, "ui.screenshot");
        assert_eq!(params, json!({"focus": false}));
    }

    #[test]
    fn other_methods_pass_unchanged() {
        let params = json!({"command": "session.inspect"});
        assert_eq!(adapt("engine.execute".into(), params.clone()), ("engine.execute".to_string(), params));
    }

    #[test]
    fn a_call_the_page_gave_up_on_is_released_so_the_app_skips_it() {
        let (waiting, control) = pending();
        drop(waiting);
        // Already abandoned: giving up again still reads "not run", and the app would skip it.
        assert!(control.dispatch.give_up().starts_with("timeout: not run"));
    }

    #[test]
    fn a_request_the_app_never_runs_answers_once_the_app_is_gone() {
        let (mut waiting, control) = pending();
        assert_eq!(waiting.poll(), None);
        drop(control);
        assert_eq!(waiting.poll().and_then(|r| r["ok"].as_bool()), Some(false), "the app went away");
    }

    #[test]
    fn an_answered_call_is_kept() {
        let (mut waiting, control) = pending();
        control.reply.send(json!({"ok": true, "result": 1})).unwrap_or_default();
        assert_eq!(waiting.poll(), Some(json!({"ok": true, "result": 1})));
    }
}
