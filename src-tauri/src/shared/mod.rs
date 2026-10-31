//! M00 共享内核:仅存放稳定且被至少两个模块使用的基础类型与工具。
//! 禁止把 ContentBlock、Anchor、Citation、ContextSnapshot、Artifact 等
//! 有明确 owner 的领域对象放入本目录(验收 M00-A6)。
//!
//! 所有权:M00。其他模块只能依赖此处的公开投影,不得反向写入。

pub mod answer_stream;
pub mod clock;
pub mod dto;
pub mod error;
pub mod events;
pub mod export_bindings;
pub mod ids;
pub mod locator;
pub mod logging;
pub mod paging;
pub mod paths;
pub mod sanitize;
pub mod settings;
pub mod stream;
pub mod textnorm;
pub mod tokens;

pub use clock::{Clock, SystemClock, TestClock};
pub use error::{new_trace_id, AppError, CoreErrorCode, UserAction};
pub use events::{DomainEvent, DomainEventType, EventSink, NoopEventSink};
pub use ids::{IdGenerator, SystemIdGenerator, TestIdGenerator};
pub use locator::{PdfRect, SourceLocator};
pub use paging::{Page, PageRequest};
pub use textnorm::{normalize_v1, NormalizedText};

/// UUIDv5 命名空间:Knowwise 内部确定性 ID。
/// 由 `UUIDv5(NAMESPACE_DNS, "knowwise.internal")` 派生,冻结后不得更改,
/// 否则历史 ContentBlock ID 会漂移。
pub const KNOWWISE_NS: uuid::Uuid =
    uuid::Uuid::from_u128(0x6b6e_7769_7365_5f69_6e74_6572_6e61_6c01_u128);
