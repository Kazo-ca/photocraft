//! The web build's copy of `craft::automation::dialogs` (storytold/craft-libs, `crates/automation/src/dialogs.rs`
//! at rev 5c5e0d52c387c0a4ae2908a07e476d7584f60362, MIT OR Apache-2.0 like PhotoCraft): craft-automation compiles
//! that module for the desktop only, while the page command API (`window.photocraft`, `?control`) drives the same
//! control methods in the browser. The desktop build keeps using craft's module (`control/dialogs.rs`,
//! `control/inspect.rs`). Only the two functions PhotoCraft calls are kept (`inspect`, `dialog_id`); PhotoCraft runs
//! `ui.dialog.*` itself (`control/dialogs.rs`). Drop this file when craft-automation builds `dialogs` for wasm32 too.
//!
//! [`inspect`] lists the open dialogs under `ui.inspect`'s `dialogs`. A dialog is named by the `id` `inspect` lists, or `"top"` for the top one; its fields are its
//! [`StackDialog::json_fields`](craft_ui::dialog::StackDialog::json_fields). Opening a dialog (`ui.dialog.open`) and
//! running the command its answer is for stay the app's: an answer waits for the app's `DialogHost::take` as a
//! click's would.

use craft::ui::dialog::{DialogHost, DialogId};
use serde_json::{Value, json};

use craft::automation::protocol::Params;

/// The open dialogs, the top one last: `{id, parent, key, fields}`, `null` for a missing parent, key or fields. Ids
/// are 64-bit hashes, so they are listed as decimal strings, which every JSON reader keeps exact.
pub fn inspect(host: &DialogHost) -> Value {
    host.open_dialogs()
        .into_iter()
        .map(|dialog| {
            json!({
                "id": dialog.id.value().to_string(),
                "parent": dialog.parent.map(|id| id.value().to_string()),
                "key": dialog.key,
                "fields": dialog.fields,
            })
        })
        .collect()
}

/// The open dialog `params.dialog` names: the id `inspect` lists, as a decimal string or a number, or `"top"` for the
/// top dialog of the stack.
pub fn dialog_id(host: &DialogHost, params: &Value) -> Result<DialogId, String> {
    let params = Params(params);
    let value = match params.0.get("dialog") {
        Some(Value::String(id)) if id == "top" => return host.stack().top().ok_or_else(|| "no hosted dialog is open".to_owned()),
        Some(Value::String(id)) => id.parse().map_err(|_| format!("`dialog` {id:?} is not an id `ui.inspect` lists"))?,
        _ => params.uint_field("dialog")?.ok_or("missing `dialog`")?,
    };
    host.stack().ids().find(|id| id.value() == value).ok_or_else(|| format!("no dialog {value}"))
}
