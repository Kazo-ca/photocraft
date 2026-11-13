//! A shell that keeps the document's file (`Services::write_document`, the web build's host mode):
//! a file handed over with its name lives there, only saves of the document go to it, each says
//! whether it is a Save As, and the document is saved only once the shell confirms the write.

use crate::file_dialog::{FileDialogAnswer, FileDialogFn, FileDialogRequest};
use crate::{PhotocraftApp, Services, menus};
use photocraft_engine::Session;
use serde_json::json;
use std::{cell::RefCell, rc::Rc};

/// (path, save_as, doc, revision) of each document save handed over.
type Handed = Rc<RefCell<Vec<(String, bool, photocraft_doc::DocId, u64)>>>;
/// Paths written through `write` (copies, exports).
type Downloaded = Rc<RefCell<Vec<String>>>;

/// A save dialog that answers like the web build: the suggested file's name.
fn web_dialog() -> FileDialogFn {
    Box::new(|request, _parent, reply| match request {
        FileDialogRequest::Save { suggested } => {
            let name = std::path::Path::new(&suggested).file_name().map_or(suggested.clone(), |n| n.to_string_lossy().into_owned());
            reply.send(Some(FileDialogAnswer::SaveTo(name)));
        }
        _ => reply.send(None),
    })
}

fn hosted_app() -> (PhotocraftApp, Handed, Downloaded) {
    let handed: Handed = Rc::default();
    let downloaded: Downloaded = Rc::default();
    let (h, d) = (handed.clone(), downloaded.clone());
    let app = PhotocraftApp::new(
        Session::new(),
        Services {
            import: Some(Box::new(|name, bytes, opts| {
                photocraft_io::import_with_options(name, bytes, opts).map(|r| (r.document, r.warnings)).map_err(|e| e.to_string())
            })),
            export: Some(Box::new(|doc, path, _| {
                photocraft_io::export(doc, path, &Default::default()).map(|r| (r.bytes, r.warnings)).map_err(|e| e.to_string())
            })),
            file_dialog: Some(web_dialog()),
            write: Some(Box::new(move |path, _| {
                d.borrow_mut().push(path.to_string());
                Ok(())
            })),
            write_unconfirmed: true,
            write_document: Some(Box::new(move |w| {
                assert!(!w.bytes.is_empty());
                h.borrow_mut().push((w.path.to_string(), w.save_as, w.doc, w.revision));
                Ok(())
            })),
            hosted: true,
            ..Default::default()
        },
    );
    (app, handed, downloaded)
}

/// A file's bytes: a 4×4 document saved as `name`, with one more layer when `layered`.
fn file(name: &str, layered: bool) -> Vec<u8> {
    let mut app = PhotocraftApp::new(Session::new(), Services::default());
    app.run("file.new", json!({"width": 4, "height": 4, "background": "white"})).unwrap();
    if layered {
        app.run("layer.new.layer", json!({})).unwrap();
    }
    photocraft_io::export(&app.session.active().unwrap().doc, name, &Default::default()).unwrap().bytes
}

fn open(app: &mut PhotocraftApp, name: &str, layered: bool) {
    app.open_bytes_at(name, &file(name, layered), name).unwrap();
    let st = app.session.active().unwrap();
    assert_eq!((st.doc.name.as_str(), st.path.as_deref()), (name, Some(name)), "the host's name is the document's file");
    assert!(!st.is_dirty());
}

fn save(app: &mut PhotocraftApp, command: &str) {
    let ctx = egui::Context::default();
    menus::invoke(app, &ctx, command, json!({})).unwrap();
    app.poll_file_dialog(&ctx, None);
}

fn last(handed: &Handed) -> (String, bool) {
    let h = handed.borrow();
    let (path, save_as, ..) = h.last().cloned().unwrap();
    (path, save_as)
}

#[test]
fn saving_a_hosted_psd_rewrites_it_and_waits_for_the_host() {
    let (mut app, handed, downloaded) = hosted_app();
    open(&mut app, "photo.psd", true);
    app.run("layer.new.layer", json!({})).unwrap();
    save(&mut app, "file.save");
    assert_eq!(last(&handed), ("photo.psd".into(), false), "written over its own file");
    assert!(downloaded.borrow().is_empty(), "the document is not downloaded");
    assert!(app.session.active().unwrap().is_dirty(), "not saved before the host confirms");
    let (_, _, doc, revision) = handed.borrow().last().cloned().unwrap();
    assert!(!app.confirm_document_write(doc, revision, "other.psd"), "a write to another file doesn't save it");
    assert!(app.confirm_document_write(doc, revision, "photo.psd"));
    assert!(!app.session.active().unwrap().is_dirty());
    assert!(!app.confirm_document_write(doc, revision, "photo.psd"), "confirmed once");
}

#[test]
fn a_flat_png_is_saved_over_itself_and_a_layered_one_as_a_psd() {
    let (mut app, handed, _) = hosted_app();
    open(&mut app, "photo.png", false);
    app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    save(&mut app, "file.save");
    assert_eq!(last(&handed), ("photo.png".into(), false), "still flat: its own file");
    app.run("layer.new.layer", json!({})).unwrap();
    save(&mut app, "file.save");
    assert_eq!(last(&handed), ("photo.psd".into(), true), "with layers it needs another file");
}

#[test]
fn save_as_asks_the_host_even_under_the_same_name() {
    let (mut app, handed, _) = hosted_app();
    open(&mut app, "photo.psd", true);
    save(&mut app, "file.saveAs");
    assert_eq!(last(&handed), ("photo.psd".into(), true));
    // A plain Save afterwards writes over its own file again.
    save(&mut app, "file.save");
    assert_eq!(last(&handed), ("photo.psd".into(), false));
}

#[test]
fn a_new_document_is_a_save_as_and_copies_still_download() {
    let (mut app, handed, downloaded) = hosted_app();
    app.run("file.new", json!({"width": 4, "height": 4, "name": "Untitled-1"})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    save(&mut app, "file.save");
    assert_eq!(last(&handed), ("Untitled-1.psd".into(), true));
    let n = handed.borrow().len();
    save(&mut app, "file.saveACopy");
    assert_eq!(handed.borrow().len(), n, "a copy is not the document");
    assert_eq!(downloaded.borrow().len(), 1, "it downloads: {:?}", downloaded.borrow());
}

#[test]
fn a_save_is_confirmed_while_another_tab_is_active() {
    let (mut app, handed, _) = hosted_app();
    open(&mut app, "photo.psd", true);
    app.run("layer.new.layer", json!({})).unwrap();
    save(&mut app, "file.save");
    let (_, _, doc, revision) = handed.borrow().last().cloned().unwrap();
    app.run("file.new", json!({"width": 4, "height": 4})).unwrap();
    assert!(app.confirm_document_write(doc, revision, "photo.psd"), "another tab being active doesn't matter");
    assert_ne!(app.session.active().unwrap().doc.id, doc, "and stays active");
}

#[test]
fn save_as_and_confirmation_rules() {
    use crate::app::{confirms, document_save_as};
    assert!(!document_save_as(false, Some("a.psd"), "a.psd"));
    assert!(document_save_as(true, Some("a.psd"), "a.psd"));
    assert!(document_save_as(false, Some("a.png"), "a.psd"));
    assert!(document_save_as(false, None, "a.psd"));
    assert!(confirms(Some("a.psd"), 3, "a.psd", 5));
    assert!(!confirms(Some("a.psd"), 5, "a.psd", 5), "already saved there");
    assert!(!confirms(Some("a.psd"), 7, "a.psd", 5), "a newer save was confirmed");
    assert!(!confirms(Some("b.psd"), 3, "a.psd", 5), "moved to another file");
    assert!(!confirms(None, u64::MAX, "a.psd", 5), "detached");
}
