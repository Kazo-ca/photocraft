//! Host mode: a page that runs PhotoCraft (as an iframe, or in its own document) opens files in it
//! and keeps what it saves, instead of the browser picker and downloads. The messages and the two
//! transports are `craft-web-host`'s (storytold/craft-libs), the contract the other Craft web
//! builds share, with the `photocraft:` prefix. This module is PhotoCraft's part of it, on the
//! model of GridCraft's.
//!
//! - `?host=parent` (an `<iframe>`) talks to the same-origin parent over `postMessage`;
//!   `data-host="page"` on the canvas (the page's own document) uses DOM `CustomEvent`s on the
//!   canvas. With no parent, or a parent on another origin, there is no bridge: the normal app.
//! - `photocraft:ready { version, appVersion, file: false }` once the app listens: send a file
//!   now. `photocraft:failed { error }` when the app couldn't start, or when the module stopped
//!   (the page's `#photocraft_stopped`).
//! - `photocraft:open { name, bytes }` opens the file in a new tab, as File › Open does, kept
//!   under its name, so File › Save writes back to it when its format keeps the document. A file
//!   over [`MAX_OPEN_BYTES`] is refused before it is copied.
//! - `photocraft:save { name, bytes, saveAs, id }` for each save of the document itself (Save,
//!   Save As). `saveAs` is `false` when the document is written over its own file, `true`
//!   otherwise (Save As, a first save, a format that can no longer hold the document). The
//!   document is saved only when the host answers `photocraft:saved { id }`; until then it stays
//!   modified. Copies, exports and preset files still download.
//! - `photocraft:dirty { dirty }` when unsaved changes appear in any open document or go away.

use std::cell::RefCell;
use std::rc::Rc;

use craft_web_host::web::{FileData, Host, Message};
use photocraft_doc::DocId;
use photocraft_ui_egui::{DocumentWrite, PhotocraftApp, Services};

use crate::hostmode::{MAX_OPEN_BYTES, Waiting, accepts_size, file_name, hosted_path};

/// The app id in the host messages (`photocraft:ready`).
const APP: &str = "photocraft";

/// What a confirmed save marks saved: the document, the revision written, and its path.
type Written = (DocId, u64, String);

thread_local! {
    /// The host, for the trap handler (`web::show_stopped`), which may run while the app is
    /// borrowed and so can't reach the bridge through it.
    static STOPPED_HOST: RefCell<Option<Rc<Host>>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct Queues {
    /// Files from the host, for the next frame: (name, bytes).
    opens: Vec<(String, Vec<u8>)>,
    /// `saved` ids from the host, for the next frame.
    saved: Vec<String>,
}

#[derive(Clone)]
pub struct Bridge {
    host: Rc<Host>,
    queues: Rc<RefCell<Queues>>,
    waiting: Rc<RefCell<Waiting<Written>>>,
}

impl Bridge {
    /// The bridge for this page, or `None` when PhotoCraft isn't hosted (see the module docs).
    pub fn detect(canvas: &web_sys::HtmlCanvasElement) -> Option<Self> {
        let host = Rc::new(Host::connect(APP, canvas)?);
        STOPPED_HOST.with(|h| {
            if let Ok(mut h) = h.try_borrow_mut() {
                *h = Some(host.clone());
            }
        });
        Some(Self { host, queues: Rc::default(), waiting: Rc::default() })
    }

    /// Document saves go to the host; listen for its files and confirmations.
    pub fn connect(&self, services: &mut Services, ctx: egui::Context) {
        self.listen(ctx);
        services.hosted = true;
        let bridge = self.clone();
        services.write_document = Some(Box::new(move |write: DocumentWrite<'_>| bridge.post_save(write)));
    }

    pub fn post_ready(&self) {
        if let Err(e) = self.host.post_ready(env!("CARGO_PKG_VERSION"), false) {
            log::error!("couldn't tell the page PhotoCraft is ready: {e}");
        }
    }

    pub fn post_failed(&self, error: &str) {
        let _ = self.host.post_failed(error, None);
    }

    /// Each frame, before the app's: open the host's files, mark confirmed saves, and tell the
    /// host when unsaved changes appear or go away (the crate doesn't repeat a state).
    pub fn sync(&self, app: &mut PhotocraftApp) {
        let (opens, saved) = {
            let Ok(mut q) = self.queues.try_borrow_mut() else { return };
            (std::mem::take(&mut q.opens), std::mem::take(&mut q.saved))
        };
        for (name, bytes) in opens {
            let opened = match hosted_path(&name) {
                Some(path) => app.open_bytes_at(&name, &bytes, path),
                None => app.open_bytes(&name, &bytes),
            };
            if let Err(e) = opened {
                log::error!("couldn't open {name} from the page: {e}");
                let _ = self.host.post_failed(&format!("couldn't open {name}: {e}"), None);
                let title = photocraft_ui_egui::i18n::fmt(photocraft_ui_egui::i18n::t("Couldn't open {name}"), &[("name", &name)]);
                photocraft_ui_egui::notices::post(app, title, vec![e], true, None);
            }
        }
        for id in saved {
            let confirmed = self.waiting.try_borrow_mut().ok().and_then(|mut w| w.confirm(&id));
            if let Some((doc, revision, path)) = confirmed {
                app.confirm_document_write(doc, revision, &path);
            }
        }
        self.host.post_dirty(app.session.documents().iter().any(|d| d.is_dirty()));
    }

    /// A save of the document, handed to the host with an id it answers with `saved`.
    fn post_save(&self, write: DocumentWrite<'_>) -> Result<(), String> {
        let name = file_name(write.path);
        let id =
            self.waiting.try_borrow_mut().map_err(|_| "a save is already being handed to the page")?.add((write.doc, write.revision, write.path.to_string()));
        if let Err(e) = self.host.post_save(&name, write.bytes, Some(&id), write.save_as) {
            if let Ok(mut w) = self.waiting.try_borrow_mut() {
                w.cancel(&id);
            }
            return Err(format!("couldn't hand {name} to the page: {e}"));
        }
        Ok(())
    }

    fn listen(&self, ctx: egui::Context) {
        let queues = self.queues.clone();
        let host = self.host.clone();
        self.host.listen(move |message| {
            let Ok(mut q) = queues.try_borrow_mut() else {
                log::warn!("a message from the page arrived while the last one was being read; dropped");
                return;
            };
            match message {
                Message::Open { name, data } => match take_open(&name, &data) {
                    Ok(bytes) => q.opens.push((name, bytes)),
                    Err(e) => {
                        log::warn!("{e}");
                        let _ = host.post_failed(&e, None);
                        return;
                    }
                },
                Message::Saved { id } => q.saved.push(id),
                Message::Host { .. } | Message::HandoffDone { .. } => return,
            }
            ctx.request_repaint();
        });
    }
}

/// The module stopped: tell the host, which can't reach the app any more. Called from the trap
/// handler; touches nothing of the app's.
pub fn post_stopped(detail: &str) {
    STOPPED_HOST.with(|h| {
        if let Ok(h) = h.try_borrow()
            && let Some(host) = h.as_ref()
        {
            let _ = host.post_failed(&format!("PhotoCraft stopped: {detail}"), None);
        }
    });
}

/// The bytes an open message carries. A host's file is untrusted input: an oversized one is
/// refused before it is copied into the module's memory (the data's own length, not the host's
/// claim).
fn take_open(name: &str, data: &FileData) -> Result<Vec<u8>, String> {
    let FileData::Bytes(array) = data else {
        return Err(format!("{name} came as a File; PhotoCraft reads bytes only (ready says file: false)"));
    };
    if !accepts_size(data.size()) {
        return Err(format!("{name} is {} bytes, over the {} MiB limit for a file from the page", data.size(), MAX_OPEN_BYTES >> 20));
    }
    Ok(array.to_vec())
}
