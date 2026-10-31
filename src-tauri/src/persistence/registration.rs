//! M01 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use super::migrations::Migration;

pub const MODULE: &str = "m01";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "init",
        sql: include_str!("../../migrations/m01/0001_init.sql"),
    }]
}
