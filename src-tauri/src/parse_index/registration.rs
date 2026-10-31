//! M04 注册元数据:模块迁移清单(M13 按依赖顺序汇总)。

use crate::persistence::Migration;

pub const MODULE: &str = "m04";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            module: MODULE,
            version: 1,
            name: "init",
            sql: include_str!("../../migrations/m04/0001_init.sql"),
        },
        // 2D 页级 OCR 预留 m21:origin='ocr' 重建 + page_ocr 页状态。
        Migration {
            module: "m21",
            version: 1,
            name: "page_ocr",
            sql: include_str!("../../migrations/m21/0001_page_ocr.sql"),
        },
        Migration {
            module: "m21",
            version: 2,
            name: "ocr_control",
            sql: include_str!("../../migrations/m21/0002_ocr_control.sql"),
        },
    ]
}
