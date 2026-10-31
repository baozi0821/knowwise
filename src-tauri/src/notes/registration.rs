//! M06 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m06";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            module: MODULE,
            version: 1,
            name: "init",
            sql: include_str!("../../migrations/m06/0001_init.sql"),
        },
        Migration {
            module: MODULE,
            version: 2,
            name: "bookmark_note",
            sql: include_str!("../../migrations/m26/0001_bookmark_note.sql"),
        },
        Migration {
            module: MODULE,
            version: 3,
            name: "annotation_clarification_remark",
            sql: include_str!("../../migrations/m06/0002_remarks.sql"),
        },
    ]
}
