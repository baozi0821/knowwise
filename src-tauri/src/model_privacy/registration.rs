//! M08 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m08";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            module: MODULE,
            version: 1,
            name: "init",
            sql: include_str!("../../migrations/m08/0001_init.sql"),
        },
        Migration {
            module: MODULE,
            version: 2,
            name: "vision_purpose",
            sql: include_str!("../../migrations/m08/0002_vision_purpose.sql"),
        },
        // 2F 长上下文:模型能力表(窗口/输出/能力标记)。
        Migration {
            module: MODULE,
            version: 3,
            name: "model_capability",
            sql: include_str!("../../migrations/m25/0001_model_capability.sql"),
        },
    ]
}
