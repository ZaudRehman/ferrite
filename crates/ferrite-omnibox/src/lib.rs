//! Omnibox: URL classification, search, autocomplete, and hints.

pub mod autocomplete;
pub mod calc;
pub mod error;
pub mod keyboard;
pub mod omnibox;
pub mod search;

pub use autocomplete::{AutocompleteCache, HistoryEntry, Suggestion};
pub use calc::instant_math;
pub use error::{OmniboxError, OmniboxResult};
pub use keyboard::{
    DEFAULT_CHARSET, HintTheme, geist_font_face_css, hint_cancel_js, hint_generation_js,
};
pub use omnibox::{OmniboxAction, OmniboxState, display_parts};
pub use search::{ENGINES, SearchEngine, SearchEngineRegistry};
