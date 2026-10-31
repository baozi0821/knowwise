//! 运行指标迁移注册:m22。

use crate::persistence::Migration;

pub const MODULE: &str = "m22";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "run_metrics",
        sql: include_str!("../../migrations/m22/0001_run_metrics.sql"),
    }]
}
