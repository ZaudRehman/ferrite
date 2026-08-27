//! Omnibox: URL classification, search, autocomplete, and hints.

pub mod autocomplete;
pub mod error;
pub mod keyboard;
pub mod omnibox;
pub mod route;
pub mod search;

pub use autocomplete::{AutocompleteCache, HistoryEntry, Suggestion};
pub use error::{OmniboxError, OmniboxResult};
pub use keyboard::{
    DEFAULT_CHARSET, HintTheme, geist_font_face_css, hint_cancel_js, hint_generation_js,
};
pub use omnibox::{OmniboxAction, OmniboxState};
pub use route::FerriteRoute;
pub use search::{ENGINES, SearchEngine, SearchEngineRegistry};
