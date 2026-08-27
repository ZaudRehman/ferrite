//! Local HTTP/HTTPS forward proxy on `127.0.0.1`.

pub mod error;
pub mod event;
pub mod payload;
pub mod server;
pub mod verdict;

pub use error::{ProxyError, ProxyResult};
pub use event::{Classification, FORK_CAP, ForkTx, ForkedPayload, NetworkEvent, TelemetryTx};
pub use payload::DebugBufferSink;
pub use server::{ProxyConfig, ProxyHandle, spawn};
pub use verdict::{
    AllowAllHook, BlockListHook, DecisionHook, RequestMeta, ResourceType, Verdict,
    resource_type_from_path,
};
