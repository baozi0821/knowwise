//! M15 注册元数据:图片附件存储(M13 按依赖顺序汇总)。
//!
//! 依赖 M02 文档、M07 会话/消息;迁移只前滚,已应用文件不可修改。

use crate::persistence::Migration;

pub const MODULE: &str = "m15";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "attachment",
        sql: include_str!("../../migrations/m15/0001_attachment.sql"),
    }]
}
