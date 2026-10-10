//! Loopback privacy proxy on 127.0.0.1. Blind CONNECT tunnels, no MITM.
//! DNS resolves here, never at OS level. Own outbound client lives in
//! `direct` and never routes through the loopback socket.

pub mod direct;
pub mod dns;
pub mod error;
pub mod event;
pub mod linkcheck;
pub mod payload;
pub mod server;
pub mod verdict;

pub use direct::{
    DIRECT_MAX_BODY, DIRECT_MAX_HEAD, DirectResponse, direct_get, leaf_pin_hex, parse_pin_entry,
    parse_pin_list, pin_allows_with, set_tls_pins, tls_pins_for,
};
pub use dns::DnsConfig;
pub use error::{ProxyError, ProxyResult};
pub use event::{
    Classification, FORK_CAP, FailureKind, ForkTx, ForkedPayload, NetworkEvent, TelemetryTx,
};
pub use linkcheck::{
    DEFAULT_LINKCHECK_CONCURRENCY, LINKCHECK_MAX_BODY, LinkStatus, MAX_LINKCHECK_CONCURRENCY,
    MAX_LINKCHECK_URLS, check_bookmark_links, check_one_link,
};
pub use payload::DebugBufferSink;
pub use server::{ArchiveProvider, ArchiveSnapshot, ProxyConfig, ProxyHandle, spawn};
pub use verdict::{
    AllowAllHook, BlockListHook, DecisionHook, RequestMeta, ResourceType, Verdict,
    resource_type_from_path,
};
