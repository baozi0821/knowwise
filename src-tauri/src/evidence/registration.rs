//! M05 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m05";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/m05/0001_init.sql"),
    }]
}
