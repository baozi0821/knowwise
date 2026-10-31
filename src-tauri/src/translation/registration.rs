//! 翻译记忆迁移注册:m20。

use crate::persistence::Migration;

pub const MODULE: &str = "m20";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        module: MODULE,
        version: 1,
        name: "translation_memory",
        sql: include_str!("../../migrations/m20/0001_translation_memory.sql"),
    }]
}
