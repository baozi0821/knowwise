//! 文档画像/卡片迁移注册:m19。

use crate::persistence::Migration;

pub const MODULE: &str = "m19";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "insights",
        sql: include_str!("../../migrations/m19/0001_insights.sql"),
    }]
}
