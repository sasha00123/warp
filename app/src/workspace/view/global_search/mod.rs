use std::ops::Deref;
use std::sync::Arc;

use warp_ripgrep::search::Submatch;
use warp_util::local_or_remote_path::LocalOrRemotePath;

pub struct SearchConfig {
    pub use_regex: bool,
    pub use_case_sensitivity: bool,
}
#[derive(Clone, Debug)]
pub struct SharedMatchText {
    text: Arc<str>,
    start: usize,
}

impl SharedMatchText {
    fn trim_leading_bytes(mut self, byte_count: usize) -> Self {
        self.start += byte_count;
        debug_assert!(self.start <= self.text.len());
        debug_assert!(self.text.is_char_boundary(self.start));
        self
    }
}

impl Deref for SharedMatchText {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text[self.start..]
    }
}

impl From<String> for SharedMatchText {
    fn from(text: String) -> Self {
        Self {
            text: text.into(),
            start: 0,
        }
    }
}

/// A single global search match: one line in one file, which may live on
/// the local filesystem or on a remote host.
#[derive(Clone, Debug)]
pub struct GlobalSearchMatch {
    pub location: LocalOrRemotePath,
    pub line_number: u32,
    /// Original 1-based character column in the file. This is captured
    /// before display-only whitespace trimming so opening a result navigates
    /// to the correct location.
    pub column_num: Option<usize>,
    pub line_text: SharedMatchText,
    pub submatches: Vec<Submatch>,
}

#[cfg_attr(not(target_family = "wasm"), path = "model.rs")]
#[cfg_attr(target_family = "wasm", path = "model_wasm.rs")]
pub mod model;
pub mod view;
