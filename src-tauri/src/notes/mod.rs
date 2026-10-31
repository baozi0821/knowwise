//! M06 标注、书签、笔记与待澄清(《M06》)。
//!
//! 保存用户阅读资产:文本高亮、书签、锚定/文档笔记、标签、状态、
//! 自动保存契约与待澄清清单。用户资产优先于后台解析、索引和 AI 任务;
//! 模块只使用 M05 创建的 Anchor,不渲染阅读器、不计算块身份。
//!
//! 阅读器显示高亮使用 M05 解析后的 AnchorOverlay 投影;列表点击通过
//! M13 注入的导航端口请求定位,本模块不 import 阅读器内部组件。

// capture:笔记 Markdown 导出(Markdown 生成纯函数默认特性下可单测,
// `#[tauri::command]` 包装在 desktop feature 之后)。
pub mod export;
pub mod models;
pub mod registration;
pub mod service;

#[cfg(feature = "desktop")]
pub mod commands;

pub use models::*;
pub use service::{
    normalize_tag_name, validate_note_body, NotesService, MAX_ANNOTATION_TEXT_CHARS,
    MAX_CLARIFICATION_CHARS, MAX_NOTE_BODY_CHARS, MAX_NOTE_TAGS, MAX_TAG_CHARS,
};
