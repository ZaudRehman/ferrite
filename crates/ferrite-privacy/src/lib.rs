//! Request verdicts: filters, threat intel, heuristics. Poisoned locks
//! fail open and count it, never block the proxy.

pub mod engine;
pub mod error;
pub mod filters;
pub mod headers;
pub mod heuristics;
pub mod js;
pub mod realtime;
pub mod spellcheck;
pub mod threat;
pub mod updater;

pub use engine::{
    BLOCK_DETAIL_TTL_MS, BLOCK_DETAILS_CAP, BlockWhy, FORM_RISK_TTL_MS, FORM_RISKS_CAP,
    FilterFetch, ORIGIN_COUNTS_CAP, OriginCounts, PAUSED_HOSTS_CAP, PrivacyEngine, PrivacySettings,
    THREAT_EXCEPTIONS_CAP, VerdictCounts,
};
pub use error::{PrivacyError, PrivacyResult};
pub use filters::{BlockTag, FilterList, ListCategory};
pub use heuristics::{
    FREQUENT_HOSTS_CAP, LOOKALIKE_MAX_DISTANCE, LOOKALIKE_MIN_HOST_LEN, edit_distance_capped,
    lookalike_match, registrable,
};
pub use spellcheck::{
    MAX_LANGS, bundled_dict_count, dict_bundle_dir, dict_file_name, dict_url, is_dict_host,
    is_dict_url, missing_dicts_in, normalize_langs, spellcheck_apply_js, spellcheck_js,
    spelling_single_word, store_dict_bytes, valid_dict_bytes,
};
