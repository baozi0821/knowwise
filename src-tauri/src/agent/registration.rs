//! Agent 迁移注册:m16(运行/步骤)+ m18(写工具确认)。
//!
//! 迁移只前滚;M13 在组装根按依赖顺序汇总。

use crate::persistence::Migration;

pub const MODULE: &str = "m16";

pub fn migrations() -> Vec<Migration> {
    vec![
        Migration {
            module: MODULE,
            version: 1,
            name: "agent_run",
            sql: include_str!("../../migrations/m16/0001_agent_run.sql"),
        },
        // P3A:扩展 agent_step.status(awaiting_confirmation)并增加确认投影。
        Migration {
            module: "m18",
            version: 1,
            name: "agent_confirmation",
            sql: include_str!("../../migrations/m18/0001_agent_confirmation.sql"),
        },
        // P0 1E:可选思考记录(默认不写;开启后纳入隐私清除)。
        Migration {
            module: "m23",
            version: 1,
            name: "agent_reasoning",
            sql: include_str!("../../migrations/m23/0001_agent_reasoning.sql"),
        },
    ]
}
