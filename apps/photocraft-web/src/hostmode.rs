//! Pure host-mode rules, shared by the bridge (`host`) and testable without a browser: the size
//! limit for a host's file, the path a hosted file is kept under, and the document saves waiting
//! for the host to say they were stored. Which pages host the app is `craft-web-host`'s.

use std::collections::VecDeque;

/// The largest file taken from the host page, refused before it is copied into the module: wasm32
/// has 4 GiB in all, and the open itself needs several times the file.
pub const MAX_OPEN_BYTES: u64 = 1 << 30;

/// Document saves still waiting for `photocraft:saved`, at most: a host that never answers can't
/// make the list grow without end (the oldest is forgotten, and its document stays modified).
pub const MAX_WAITING_SAVES: usize = 64;

/// The name `craft-web-host` gives an open request that carries no usable one (it cleans a host's
/// name and falls back to this).
const NAMELESS: &str = "document";

/// Whether a file of `size` bytes from the host may be opened.
pub fn accepts_size(size: f64) -> bool {
    size.is_finite() && size >= 0.0 && size <= MAX_OPEN_BYTES as f64
}

/// The path a file from the host is kept under: its name, so File › Save writes back to it. A
/// file with no usable name has none, and its first Save asks for one, as for a new document.
pub fn hosted_path(name: &str) -> Option<&str> {
    (name != NAMELESS).then_some(name)
}

/// The file name of a save path (the web's paths are names already; a folder is dropped).
pub fn file_name(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

/// Document saves handed to the host, waiting for its `saved`: each `id` with what it confirms
/// (the document, its revision, and the path it was written to).
pub struct Waiting<D> {
    next: u64,
    saves: VecDeque<(String, D)>,
}

impl<D> Default for Waiting<D> {
    fn default() -> Self {
        Self { next: 1, saves: VecDeque::new() }
    }
}

impl<D> Waiting<D> {
    /// A new id for a save of `what`, kept until [`Self::confirm`]. The oldest is forgotten past
    /// [`MAX_WAITING_SAVES`].
    pub fn add(&mut self, what: D) -> String {
        let id = format!("save-{}", self.next);
        self.next = self.next.saturating_add(1);
        if self.saves.len() >= MAX_WAITING_SAVES {
            self.saves.pop_front();
        }
        self.saves.push_back((id.clone(), what));
        id
    }

    /// Forgets a save the host could not be told about.
    pub fn cancel(&mut self, id: &str) {
        self.saves.retain(|(i, _)| i != id);
    }

    /// What the host's `saved { id }` confirms, once: `None` for an id it never got or already
    /// answered.
    pub fn confirm(&mut self, id: &str) -> Option<D> {
        let at = self.saves.iter().position(|(i, _)| i == id)?;
        self.saves.remove(at).map(|(_, what)| what)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.saves.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_file_over_the_limit_or_with_a_bad_size_is_refused() {
        assert!(accepts_size(0.0));
        assert!(accepts_size(MAX_OPEN_BYTES as f64));
        assert!(!accepts_size(MAX_OPEN_BYTES as f64 + 1.0));
        assert!(!accepts_size(f64::NAN));
        assert!(!accepts_size(-1.0));
        assert!(!accepts_size(f64::INFINITY));
    }

    #[test]
    fn a_named_file_lives_under_its_name_and_a_nameless_one_has_no_path() {
        assert_eq!(hosted_path("photo.psd"), Some("photo.psd"));
        assert_eq!(hosted_path("document"), None, "what the crate says for a missing or blank name");
    }

    #[test]
    fn save_paths_become_file_names() {
        assert_eq!(file_name("photo.psd"), "photo.psd");
        assert_eq!(file_name("dir/photo.psd"), "photo.psd");
    }

    #[test]
    fn each_save_is_confirmed_once_by_its_own_id() {
        let mut waiting = Waiting::default();
        let a = waiting.add("a");
        let b = waiting.add("b");
        assert_ne!(a, b);
        assert_eq!(waiting.confirm(&b), Some("b"));
        assert_eq!(waiting.confirm(&b), None, "answered already");
        assert_eq!(waiting.confirm("save-999"), None, "never sent");
        waiting.cancel(&a);
        assert_eq!(waiting.confirm(&a), None, "the host was never told");
    }

    #[test]
    fn a_host_that_never_answers_cannot_grow_the_list() {
        let mut waiting = Waiting::default();
        let first = waiting.add(0);
        for i in 1..=MAX_WAITING_SAVES {
            waiting.add(i);
        }
        assert_eq!(waiting.len(), MAX_WAITING_SAVES);
        assert_eq!(waiting.confirm(&first), None, "the oldest is forgotten");
    }
}
