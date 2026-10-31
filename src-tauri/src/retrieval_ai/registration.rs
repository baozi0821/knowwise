//! M09 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m09";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/m09/0001_init.sql"),
    }]
}
