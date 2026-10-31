//! 命令注册总表(M13 拥有,`desktop` feature)。Tauri 只允许一个
//! `invoke_handler`,所有模块的命令在这里一次性汇总;模块只提供命令函数。
//!
//! `knowwise_commands!` 宏是唯一事实来源:同一份清单同时展开为
//! `invoke_handler` 与 `COMMAND_NAMES`,命令名契约测试
//! (`command_names.rs`)直接解析本文件源码,两侧不可能漂移。
//!
//! M13 冻结初始化顺序(《M13》§2):日志 → 单实例 → 设置文件 →
//! 数据库(quick_check → 先备份再迁移)→ 任务恢复 → 服务托管。
//! 启动失败托管 `ShellStartup::failed`,前端进入恢复页,进程不崩溃。

use tauri::Manager;

use super::bootstrap_with;
use super::desktop::{resolve_config_dir, resolve_data_dir, TauriEventSink};
use super::shell::{ShellStartup, ShellStatusView};

/// 命令名清单(供契约测试与前端防漂移;与下方宏体一一对应)。
macro_rules! knowwise_commands {
    ($($name:literal => $cmd:path),* $(,)?) => {
        /// 注册的全部命令名,顺序与 invoke_handler 完全一致。
        pub const COMMAND_NAMES: &[&str] = &[$($name),*];

        /// 把完整命令清单挂到 Builder 上(唯一 invoke_handler)。
        #[allow(clippy::redundant_closure_for_method_calls)]
        fn attach_commands(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
            builder.invoke_handler(tauri::generate_handler![$($cmd),*])
        }
    };
}

knowwise_commands! {
    // M01
    "task_list" => crate::persistence::commands::task_list,
    "task_delete" => crate::persistence::commands::task_delete,
    // M13(壳层状态;恢复页依赖它,任何启动失败下都可用)
    "shell_get_status" => shell_get_status,
    // capture(屏幕框选截图:原生整屏捕获)
    "capture_screen" => crate::app_shell::screenshot::capture_screen,
    // M02
    "document_import" => crate::document::commands::document_import,
    "document_delete" => crate::document::commands::document_delete,
    "document_list_recent" => crate::document::commands::document_list_recent,
    "document_get" => crate::document::commands::document_get,
    "document_get_suggested_prompts" => crate::document::commands::document_get_suggested_prompts,
    "document_open" => crate::document::commands::document_open,
    "document_check_file" => crate::document::commands::document_check_file,
    "document_reconnect" => crate::document::commands::document_reconnect,
    "document_confirm_version" => crate::document::commands::document_confirm_version,
    // M03(阅读器)
    "reader_get_render_plan" => crate::document::reader_commands::reader_get_render_plan,
    "reader_read_range" => crate::document::reader_commands::reader_read_range,
    "reader_save_position" => crate::document::reader_commands::reader_save_position,
    "reader_get_position" => crate::document::reader_commands::reader_get_position,
    // M04(解析与索引;parser_* 为解析桥内部命令,仅 Worker 调用)
    "parse_start" => crate::parse_index::commands::parse_start,
    "parse_get_status" => crate::parse_index::commands::parse_get_status,
    "parse_cancel" => crate::parse_index::commands::parse_cancel,
    "parse_retry" => crate::parse_index::commands::parse_retry,
    "parser_read_input" => crate::parse_index::commands::parser_read_input,
    "parser_submit_batch" => crate::parse_index::commands::parser_submit_batch,
    "parser_complete" => crate::parse_index::commands::parser_complete,
    "parser_fail" => crate::parse_index::commands::parser_fail,
    "parser_submit_embeddings" => crate::parse_index::commands::parser_submit_embeddings,
    // 2D(页级 OCR;report 仅 Worker 令牌可达)
    "page_ocr_report" => crate::parse_index::commands::page_ocr_report,
    "page_ocr_get_status" => crate::parse_index::commands::page_ocr_get_status,
    "page_ocr_pause" => crate::parse_index::commands::page_ocr_pause,
    "page_ocr_resume" => crate::parse_index::commands::page_ocr_resume,
    "page_ocr_control_get" => crate::parse_index::commands::page_ocr_control_get,
    "page_ocr_get_text" => crate::parse_index::commands::page_ocr_get_text,
    "page_ocr_apply_correction" => crate::parse_index::commands::page_ocr_apply_correction,
    "page_ocr_save_recognition" => crate::parse_index::commands::page_ocr_save_recognition,
    "page_ocr_reprocess" => crate::parse_index::commands::page_ocr_reprocess,
    // M05(锚点与证据)
    "evidence_create_anchor" => crate::evidence::commands::evidence_create_anchor,
    "evidence_resolve_citation" => crate::evidence::commands::evidence_resolve_citation,
    "evidence_resolve_block" => crate::evidence::commands::evidence_resolve_block,
    "evidence_get_lens" => crate::evidence::commands::evidence_get_lens,
    "evidence_feedback" => crate::evidence::commands::evidence_feedback,
    "evidence_confirm_support" => crate::evidence::commands::evidence_confirm_support,
    "evidence_propose_relocation" => crate::evidence::commands::evidence_propose_relocation,
    "evidence_confirm_relocation" => crate::evidence::commands::evidence_confirm_relocation,
    "evidence_reject_relocation" => crate::evidence::commands::evidence_reject_relocation,
    // M06(标注、书签、笔记与待澄清)
    "annotation_create" => crate::notes::commands::annotation_create,
    "annotation_update" => crate::notes::commands::annotation_update,
    "annotation_delete" => crate::notes::commands::annotation_delete,
    "annotation_list" => crate::notes::commands::annotation_list,
    "bookmark_toggle" => crate::notes::commands::bookmark_toggle,
    "bookmark_list" => crate::notes::commands::bookmark_list,
    "bookmark_rename" => crate::notes::commands::bookmark_rename,
    "note_save" => crate::notes::commands::note_save,
    "note_list" => crate::notes::commands::note_list,
    "note_get" => crate::notes::commands::note_get,
    "note_delete" => crate::notes::commands::note_delete,
    // capture(笔记 Markdown 导出)
    "notes_export_markdown" => crate::notes::export::notes_export_markdown,
    "clarification_create" => crate::notes::commands::clarification_create,
    "clarification_list" => crate::notes::commands::clarification_list,
    "clarification_update" => crate::notes::commands::clarification_update,
    "clarification_set_remark" => crate::notes::commands::clarification_set_remark,
    "clarification_delete" => crate::notes::commands::clarification_delete,
    // M11(理解校验题:作答、无效题反馈与题目中心读模型;
    // quiz.v1 内容本体由 M10 ArtifactService 保存)
    "quiz_generate" => crate::app_shell::generation_flow::quiz_generate,
    "quiz_generate_v2" => crate::app_shell::generation_flow::quiz_generate_v2,
    "quiz_regenerate" => crate::app_shell::generation_flow::quiz_regenerate,
    "quiz_submit_answer" => crate::quiz::commands::quiz_submit_answer,
    "quiz_submit_answer_v2" => crate::quiz::commands::quiz_submit_answer_v2,
    "quiz_feedback" => crate::quiz::commands::quiz_feedback,
    "quiz_feedback_update" => crate::quiz::commands::quiz_feedback_update,
    "quiz_list_records" => crate::quiz::commands::quiz_list_records,
    "quiz_list_feedback" => crate::quiz::commands::quiz_list_feedback,
    "quiz_version_summary" => crate::quiz::commands::quiz_version_summary,
    // M07(上下文快照与会话)
    "context_suggest" => crate::context::commands::context_suggest,
    "context_preview" => crate::context::commands::context_preview,
    "context_freeze" => crate::context::commands::context_freeze,
    "context_get" => crate::context::commands::context_get,
    "conversation_create" => crate::context::commands::conversation_create,
    "conversation_list" => crate::context::commands::conversation_list,
    "conversation_get_messages" => crate::context::commands::conversation_get_messages,
    "message_get" => crate::context::commands::message_get,
    "conversation_set_active_snapshot" => crate::context::commands::conversation_set_active_snapshot,
    "conversation_rename" => crate::context::commands::conversation_rename,
    "conversation_pin" => crate::context::commands::conversation_pin,
    "conversation_branch" => crate::context::commands::conversation_branch,
    "conversation_delete" => crate::context::commands::conversation_delete,
    "conversation_export" => crate::context::commands::conversation_export,
    // P2 阅读简报(单文件 HTML/Markdown;只聚合真实标注与问答)
    "reading_brief_export" => crate::brief::reading_brief_export,
    "message_append_user" => crate::context::commands::message_append_user,
    "message_append_assistant" => crate::context::commands::message_append_assistant,
    // M15(图片附件)
    "attachment_upload" => crate::attachments::commands::attachment_upload,
    "attachment_import_local" => crate::attachments::commands::attachment_import_local,
    "attachment_thumbnail" => crate::attachments::commands::attachment_thumbnail,
    "attachment_clone" => crate::attachments::commands::attachment_clone,
    "attachment_delete" => crate::attachments::commands::attachment_delete,
    // M08(模型接入、隐私与设置)
    "vision_get_status" => crate::model_privacy::commands::vision_get_status,
    "vision_allow_remote" => crate::model_privacy::commands::vision_allow_remote,
    "model_get_status" => crate::model_privacy::commands::model_get_status,
    "model_capability_get" => crate::model_privacy::commands::model_capability_get,
    "model_capability_set" => crate::model_privacy::commands::model_capability_set,
    "model_list_local_models" => crate::model_privacy::commands::model_list_local_models,
    "model_set_reference" => crate::model_privacy::commands::model_set_reference,
    "remote_secret_set" => crate::model_privacy::commands::remote_secret_set,
    "remote_secret_delete" => crate::model_privacy::commands::remote_secret_delete,
    "privacy_get_status" => crate::model_privacy::commands::privacy_get_status,
    "privacy_grant" => crate::model_privacy::commands::privacy_grant,
    "privacy_revoke" => crate::model_privacy::commands::privacy_revoke,
    "privacy_preview_clear" => crate::model_privacy::commands::privacy_preview_clear,
    "privacy_execute_clear" => crate::model_privacy::commands::privacy_execute_clear,
    "settings_get" => crate::model_privacy::commands::settings_get,
    "settings_update" => crate::model_privacy::commands::settings_update,
    "diagnostics_preview" => crate::model_privacy::commands::diagnostics_preview,
    "diagnostics_export" => crate::model_privacy::commands::diagnostics_export,
    // M09(检索与问答编排;ai_ask 为 M13 编排:M07 消息 × M09 问答)
    "ai_ask" => crate::app_shell::ask_flow::ai_ask,
    "ai_ask_streamed" => crate::app_shell::ask_flow::ai_ask_streamed,
    "ai_quick_answer" => crate::app_shell::ask_flow::ai_quick_answer,
    "ai_get_task" => crate::retrieval_ai::commands::ai_get_task,
    "ai_cancel" => crate::retrieval_ai::commands::ai_cancel,
    "ai_retry" => crate::retrieval_ai::commands::ai_retry,
    // 任务中心(重试真实重发原问题/原快照;不覆盖旧消息)
    "task_retry" => crate::app_shell::ask_flow::task_retry,
    // P2(Agent 只读循环;默认问答仍走 ai_ask)
    "agent_run" => crate::agent::commands::agent_run,
    "agent_run_streamed" => crate::agent::commands::agent_run_streamed,
    "agent_get_steps" => crate::agent::commands::agent_get_steps,
    "agent_cancel" => crate::agent::commands::agent_cancel,
    "agent_confirm_tool" => crate::agent::commands::agent_confirm_tool,
    // M10(产物中心与可编辑摘要;M13 后台任务提交/取消/只读轮询)
    "artifact_generate" => crate::app_shell::generation_flow::artifact_generate,
    "generation_cancel" => crate::app_shell::generation_flow::generation_cancel,
    "generation_get" => crate::app_shell::generation_flow::generation_get,
    "artifact_regenerate" => crate::app_shell::generation_flow::artifact_regenerate,
    "artifact_commit_draft" => crate::artifacts::commands::artifact_commit_draft,
    "artifact_list" => crate::artifacts::commands::artifact_list,
    "artifact_get" => crate::artifacts::commands::artifact_get,
    "artifact_rename" => crate::artifacts::commands::artifact_rename,
    "artifact_copy" => crate::artifacts::commands::artifact_copy,
    "artifact_begin_edit" => crate::artifacts::commands::artifact_begin_edit,
    "artifact_save" => crate::artifacts::commands::artifact_save,
    "artifact_seal" => crate::artifacts::commands::artifact_seal,
    "artifact_restore" => crate::artifacts::commands::artifact_restore,
    "artifact_delete_preview" => crate::artifacts::commands::artifact_delete_preview,
    "artifact_delete" => crate::artifacts::commands::artifact_delete,
    "artifact_export_preview" => crate::artifacts::commands::artifact_export_preview,
    "artifact_export" => crate::artifacts::commands::artifact_export,
    "artifact_save_citations" => crate::artifacts::commands::artifact_save_citations,
    // 2C(翻译;translation.v1;M13 后台任务提交/只读轮询/取消)
    "translation_generate" => crate::translation::commands::translation_generate,
    "translation_get" => crate::translation::commands::translation_get,
    "translation_cancel" => crate::translation::commands::translation_cancel,
    "translation_inline" => crate::translation::commands::translation_inline,
    "translation_local_status" => crate::translation::commands::translation_local_status,
    // 2A(文档画像/动态卡片;缓存可重建,每条事实带来源)
    "insight_cards_get" => crate::insights::commands::insight_cards_get,
    "insight_cards_refresh" => crate::insights::commands::insight_cards_refresh,
    "document_profile_get" => crate::insights::commands::document_profile_get,
    // M13(运行指标:只读诊断投影,不含正文)
    "metrics_recent" => crate::metrics::commands::metrics_recent,
    // M12(流程论证图:内容级校验/布局/大纲/导出;
    // Artifact 身份与版本由 M10 保存)
    "graph_validate" => crate::graph::commands::graph_validate,
    "graph_layout" => crate::graph::commands::graph_layout,
    "graph_outline" => crate::graph::commands::graph_outline,
    "graph_export" => crate::graph::commands::graph_export,
}

/// 壳层状态查询:不依赖 AppState,启动失败(恢复页)时仍可调用。
#[tauri::command]
pub fn shell_get_status(state: tauri::State<'_, ShellStartup>) -> ShellStatusView {
    state.status.clone()
}

/// 构建并运行桌面应用。setup 永不返回 Err:启动失败进入恢复页状态,
/// 由前端 shell_get_status 呈现;单实例回调聚焦已有窗口(《M13》§10)。
pub fn run() {
    super::desktop::init_logging();

    let builder = tauri::Builder::default()
        // 单实例必须最先注册:第二个进程不落库,仅聚焦已有窗口,
        // 防止两个写入进程同时打开同一应用数据库(M13-A8)。
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        // 受控系统入口:文件选择、剪贴板写入、外部链接确认打开。
        // 能力清单只放行最小权限(security_config.rs 契约)。
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = resolve_data_dir(&handle);
            let config_dir = resolve_config_dir(&handle);
            match bootstrap_with(
                &data_dir,
                &config_dir,
                std::sync::Arc::new(TauriEventSink::new(handle.clone())),
            ) {
                Ok(state) => {
                    tracing::info!(status = "ready", "app shell bootstrapped");
                    app.manage(ShellStartup::ready());
                    app.manage(state);
                }
                Err(error) => {
                    // 恢复页路径:原文件与迁移前备份保留,业务零写入。
                    tracing::error!(
                        error_code = error.code.clone(),
                        status = "failed",
                        "app shell bootstrap failed"
                    );
                    app.manage(ShellStartup::failed(error));
                }
            }
            Ok(())
        });
    attach_commands(builder)
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
