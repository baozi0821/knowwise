//! 命令名清单契约(M13,《02》§3 已实现子集)。
//!
//! `command_registry.rs` 中的 `knowwise_commands!` 宏是唯一事实来源:
//! 宏同时展开为 `invoke_handler` 注册表和本模块解析出的名字清单,
//! 两者同源、不可能漂移。本文件在默认特性(无 Tauri)下也编译,
//! 使 `cargo test` 即可运行命令清单契约测试。

/// 从 `command_registry.rs` 源码解析宏清单,返回 (命令名, 命令函数路径)。
///
/// 解析约定(与 command_registry.rs 的格式一一对应):
/// - 宏调用形如 `knowwise_commands! {`,宏体以单独一行 `}` 结束;
/// - 每个条目独占一行 `"command_name" => crate::module::commands::command_name,`;
/// - `//` 开头的行为注释行,跳过。
pub fn parse_registered_commands(source: &str) -> Result<Vec<(String, String)>, String> {
    const MACRO_HEAD: &str = "knowwise_commands! {";
    let head = source
        .find(MACRO_HEAD)
        .ok_or_else(|| "未找到 knowwise_commands! 宏调用".to_string())?;
    let body_start = head + MACRO_HEAD.len();
    let body_raw = &source[body_start..];
    // 宏体以行首无缩进的 "}" 结束(generate_handler 宏体内没有嵌套大括号块)。
    let body_end = body_raw
        .find("\n}")
        .ok_or_else(|| "宏体缺少结束大括号".to_string())?;
    let body = &body_raw[..body_end];

    let mut entries = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        let Some((name_part, path_part)) = line.split_once("=>") else {
            return Err(format!("宏体存在无法解析的行:{line}"));
        };
        let name = name_part.trim().trim_matches('"');
        let path = path_part.trim().trim_end_matches(',').trim();
        if name.is_empty() || path.is_empty() {
            return Err(format!("宏体存在空命令名或空路径:{line}"));
        }
        entries.push((name.to_string(), path.to_string()));
    }
    if entries.is_empty() {
        return Err("命令清单为空".to_string());
    }
    Ok(entries)
}

/// 宏体中的全部命令名(按注册顺序)。
pub fn registered_command_names(source: &str) -> Result<Vec<String>, String> {
    Ok(parse_registered_commands(source)?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGISTRY_SOURCE: &str = include_str!("command_registry.rs");

    /// 《02》§3 已实现命令的冻结集合。新增命令必须同步本清单,
    /// 这正是契约测试的意义:命令表只增不减、名实一致。
    const EXPECTED_CONTRACT: &[&str] = &[
        // M01
        "task_list",
        "task_delete",
        "task_retry",
        // M13(壳层状态;数据库失败时前端唯一可调用的命令)
        "shell_get_status",
        // capture(屏幕框选截图:原生整屏捕获)
        "capture_screen",
        // M02
        "document_import",
        "document_delete",
        "document_list_recent",
        "document_get",
        "document_get_suggested_prompts",
        "document_open",
        "document_check_file",
        "document_reconnect",
        "document_confirm_version",
        // M03(阅读器)
        "reader_get_render_plan",
        "reader_read_range",
        "reader_save_position",
        "reader_get_position",
        // M04(解析与索引;parser_* 为解析桥内部命令,仅 Worker 调用)
        "parse_start",
        "parse_get_status",
        "parse_cancel",
        "parse_retry",
        "parser_read_input",
        "parser_submit_batch",
        "parser_complete",
        "parser_fail",
        "parser_submit_embeddings",
        // 2D(页级 OCR)
        "page_ocr_report",
        "page_ocr_get_status",
        "page_ocr_pause",
        "page_ocr_resume",
        "page_ocr_control_get",
        "page_ocr_get_text",
        "page_ocr_apply_correction",
        "page_ocr_save_recognition",
        "page_ocr_reprocess",
        // M05(锚点与证据)
        "evidence_create_anchor",
        "evidence_resolve_citation",
        "evidence_resolve_block",
        "evidence_get_lens",
        "evidence_feedback",
        "evidence_confirm_support",
        "evidence_propose_relocation",
        "evidence_confirm_relocation",
        "evidence_reject_relocation",
        // M06(标注、书签、笔记与待澄清)
        "annotation_create",
        "annotation_update",
        "annotation_delete",
        "annotation_list",
        "bookmark_toggle",
        "bookmark_list",
        "bookmark_rename",
        "note_save",
        "note_list",
        "note_get",
        "note_delete",
        // capture(笔记 Markdown 导出)
        "notes_export_markdown",
        "clarification_create",
        "clarification_list",
        "clarification_update",
        "clarification_set_remark",
        "clarification_delete",
        // M11(理解校验题)
        "quiz_generate",
        "quiz_generate_v2",
        "quiz_regenerate",
        "quiz_submit_answer",
        "quiz_submit_answer_v2",
        "quiz_feedback",
        "quiz_feedback_update",
        "quiz_list_records",
        "quiz_list_feedback",
        "quiz_version_summary",
        // M07(上下文快照与会话)
        "context_suggest",
        "context_preview",
        "context_freeze",
        "context_get",
        "conversation_create",
        "conversation_list",
        "conversation_get_messages",
        "message_get",
        "conversation_set_active_snapshot",
        "conversation_rename",
        "conversation_pin",
        "conversation_branch",
        "conversation_delete",
        "conversation_export",
        "reading_brief_export",
        "message_append_user",
        "message_append_assistant",
        // M15(图片附件)
        "attachment_upload",
        "attachment_import_local",
        "attachment_thumbnail",
        "attachment_clone",
        "attachment_delete",
        // M08(模型接入、隐私与设置)
        "vision_get_status",
        "vision_allow_remote",
        "model_get_status",
        "model_capability_get",
        "model_capability_set",
        "model_list_local_models",
        "model_set_reference",
        "remote_secret_set",
        "remote_secret_delete",
        "privacy_get_status",
        "privacy_grant",
        "privacy_revoke",
        "privacy_preview_clear",
        "privacy_execute_clear",
        "settings_get",
        "settings_update",
        "diagnostics_preview",
        "diagnostics_export",
        // M09(检索与问答编排;streamed 入口用 Channel 推送真实事件)
        "ai_ask",
        "ai_ask_streamed",
        "ai_quick_answer",
        "ai_get_task",
        "ai_cancel",
        "ai_retry",
        // P2(Agent 只读循环)
        "agent_run",
        "agent_run_streamed",
        "agent_get_steps",
        "agent_cancel",
        "agent_confirm_tool",
        // M10(产物中心与可编辑摘要)
        "artifact_generate",
        "generation_cancel",
        "generation_get",
        "artifact_regenerate",
        "artifact_commit_draft",
        "artifact_list",
        "artifact_get",
        "artifact_rename",
        "artifact_copy",
        "artifact_begin_edit",
        "artifact_save",
        "artifact_seal",
        "artifact_restore",
        "artifact_delete_preview",
        "artifact_delete",
        "artifact_export_preview",
        "artifact_export",
        "artifact_save_citations",
        // M12(流程论证图:内容级校验/布局/大纲/导出)
        "graph_validate",
        "graph_layout",
        "graph_outline",
        "graph_export",
        // 2C(翻译;M13 后台任务提交/只读轮询/取消)
        "translation_generate",
        "translation_get",
        "translation_cancel",
        "translation_inline",
        "translation_local_status",
        // 2A(文档画像/动态卡片)
        "insight_cards_get",
        "insight_cards_refresh",
        "document_profile_get",
        // M13(运行指标)
        "metrics_recent",
    ];

    #[test]
    fn manifest_has_no_duplicate_commands() {
        let names = registered_command_names(REGISTRY_SOURCE).unwrap();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(names.len(), sorted.len(), "命令清单存在重复注册:{names:?}");
    }

    #[test]
    fn manifest_names_are_snake_case() {
        let names = registered_command_names(REGISTRY_SOURCE).unwrap();
        for name in &names {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "命令名 {name} 不符合 snake_case"
            );
            assert!(
                !name.starts_with(|c: char| c.is_ascii_digit()),
                "命令名 {name} 不能以数字开头"
            );
        }
    }

    #[test]
    fn manifest_name_matches_command_function_path() {
        let entries = parse_registered_commands(REGISTRY_SOURCE).unwrap();
        for (name, path) in &entries {
            let last = path.rsplit("::").next().unwrap_or("");
            assert_eq!(
                name, last,
                "命令名 {name} 与函数路径末段 {last} 不一致(名实不符)"
            );
        }
    }

    /// M13-A3/《02》§3:注册总表与契约命令清单一致(只增不减)。
    #[test]
    fn manifest_matches_frozen_contract_set() {
        let names = registered_command_names(REGISTRY_SOURCE).unwrap();
        let mut expected = EXPECTED_CONTRACT.to_vec();
        let mut actual = names.clone();
        expected.sort();
        actual.sort();
        assert_eq!(
            actual, expected,
            "命令注册表与《02》契约清单不一致;新增命令需同步两侧"
        );
    }

    #[test]
    fn generation_commands_use_the_background_orchestration() {
        let entries = parse_registered_commands(REGISTRY_SOURCE).unwrap();
        for name in [
            "artifact_generate",
            "quiz_generate_v2",
            "generation_get",
            "generation_cancel",
            "quiz_generate",
            "artifact_regenerate",
            "quiz_regenerate",
        ] {
            assert!(entries.contains(&(
                name.to_string(),
                format!("crate::app_shell::generation_flow::{name}"),
            )));
        }
    }

    #[test]
    fn translation_commands_use_the_background_orchestration() {
        let entries = parse_registered_commands(REGISTRY_SOURCE).unwrap();
        for name in [
            "translation_generate",
            "translation_get",
            "translation_cancel",
        ] {
            assert!(entries.contains(&(
                name.to_string(),
                format!("crate::translation::commands::{name}"),
            )));
        }
    }

    #[test]
    fn parser_rejects_malformed_manifest() {
        assert!(parse_registered_commands("fn main() {}").is_err());
        assert!(parse_registered_commands("knowwise_commands! {\n}").is_err());
        // 名字与路径不一致会在名实测试中被抓;解析器本身只保证结构。
        let ok = "knowwise_commands! {\n    \"task_list\" => crate::persistence::commands::task_list,\n}";
        assert_eq!(
            parse_registered_commands(ok).unwrap(),
            vec![(
                "task_list".to_string(),
                "crate::persistence::commands::task_list".to_string()
            )]
        );
    }
}
