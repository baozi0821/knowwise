//! M12 注册元数据(M13 按依赖顺序汇总)。
//!
//! M12 不拥有业务内容表:图内容保存在 M10 的
//! `artifact_version.content_json`(《M12》§1 数据库归属),
//! 迁移清单为空;M10 表结构由 M10 落地。

use crate::persistence::Migration;

pub const MODULE: &str = "m12";

pub fn migrations() -> Vec<Migration> {
    Vec::new()
}
