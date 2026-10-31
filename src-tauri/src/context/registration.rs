//! M07 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m07";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            module: MODULE,
            version: 1,
            name: "init",
            sql: include_str!("../../migrations/m07/0001_init.sql"),
        },
        // M17:会话滚动摘要(WBS-5)。独立模块号只前滚;表归 M07 读取/写入。
        Migration {
            module: "m17",
            version: 1,
            name: "conversation_summary",
            sql: include_str!("../../migrations/m17/0001_conversation_summary.sql"),
        },
        // 会话置顶(前滚;只增加列,不改旧数据)。
        Migration {
            module: "m27",
            version: 1,
            name: "conversation_pinned",
            sql: include_str!("../../migrations/m27/0001_conversation_pinned.sql"),
        },
    ]
}
