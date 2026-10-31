//! M04 语义解析与索引(《M04》)。
//!
//! 从文件到语义数据的全部写入归本模块独占:RenderAST 输入经解析桥
//! (不受信 Worker + 短期令牌)进入,服务端裁决批次边界、规范化、
//! 确定性块 ID、覆盖真相(ParseRegion)、FTS 与向量代次。
//!
//! 阅读与语义两条链路共享 DocumentVersion 但状态独立:首屏阅读不等待
//! ParseGeneration,解析失败不破坏已保存笔记、产物与阅读能力。

pub mod fts;
pub mod models;
pub mod registration;
pub mod service;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::{
    BlockCompleteness, BlockKind, BlockOrigin, Capability, ContentBlockView, CoverageView, FtsHit,
    FtsState, ParseJobTicket, ParseReadMode, ParseState, ParseStatusView, ParserBatchInput,
    ParserBlockInput, ParserRegionInput, ParserUnsupportedInput, RegionKind, RegionStatus,
    RegionStatusView, SelectionBlockRequest, UnsupportedRegionView, VectorState,
};
pub use service::{
    parser_config_hash_of, BatchAck, EmbeddingInput, ParseIndexService, ParseJobBinding,
    ParserVersions, VectorHit, MAX_BATCH_TEXT_BYTES, MAX_BLOCKS_PER_BATCH, MAX_READ_CHUNK_BYTES,
    NORMALIZATION_VERSION,
};
