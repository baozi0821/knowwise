//! M11 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m11";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            module: MODULE,
            version: 1,
            name: "init",
            sql: include_str!("../../migrations/m11/0001_init.sql"),
        },
        Migration {
            module: "m24",
            version: 1,
            name: "quiz_v2_attempt",
            sql: include_str!("../../migrations/m24/0001_quiz_v2_attempt.sql"),
        },
    ]
}
