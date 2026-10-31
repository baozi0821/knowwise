//! M09 检索与问答编排服务(《M09》§2–§10)。
//!
//! 编排顺序:任务 → 快照/能力/预算检查(模型前终止,A8)→ 检索
//! (范围过滤先于排名,A1/A2)→ Prompt 装填(来源分层,§6)→
//! 结构化生成(Port)→ AnswerContent 校验(§7)→ 同事务提交
//! Manifest 与 Citation + TaskResult(§2)。
//!
//! M09 不保存回答正文(归 M07 消息)、不解析文档、不写索引、不创建 Artifact。

use std::collections::HashSet;
use std::sync::Arc;

use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::evidence::models::{
    CitationInput, CitationOwnerType, CitationSourceType, ClaimKind, SaveCitationsRequest,
};
use crate::evidence::service::{save_citations_tx, OwnerValidators};
use crate::evidence::{EvidenceService, OwnerCheckFn};
use crate::parse_index::models::ContentBlockView;
use crate::parse_index::{ParseIndexService, VectorHit};
use crate::persistence::task::{AttemptRow, CommitOutcome, CreateTask, TaskRow, TaskService};
use crate::persistence::Db;
use crate::shared::dto::{TaskOutcome, TaskPhase, TaskStatus, TaskType};
use crate::shared::error::AppError;
use crate::shared::ids::IdGenerator;
use crate::shared::stream::{noop_observer, SharedObserver, StreamStage};
use crate::shared::textnorm::sha256_str;
use crate::shared::{Clock, CoreErrorCode, EventSink};

use super::budget::{self, TokenBudget};
use super::error as m09err;
use super::fusion::{
    decide_candidates, rrf_fuse, CandidateDecision, FusedCandidate, RankedList, RetrievalConfig,
    DEFAULT_CONFIG,
};
use super::history::{build_conversation_summary_request, parse_conversation_summary};
use super::models::{
    parse_answer_content, AnswerCitation, AnswerContent, AnswerStatus, ChatTurn, ChatTurnRole,
    GenerationInput, SegmentKind, SourceRole, ANSWER_SCHEMA_VERSION,
};
use super::ports::{
    resolve_capability_mode, CapabilityMode, ContextSnapshotPort, ConversationSummaryPort,
    ModelCapabilityView, SnapshotContext, SnapshotScopeKind, StructuredGenerationPort,
};
use super::prompt::{
    assemble_prompt_with_token_budget, build_qa_request, estimate_tokens, system_instruction_for,
    PromptAssembly, MAX_PROMPT_BLOCKS,
};
use super::query::{parse_query_with_previous, ParsedQuery};

/// 问答任务结果(写入 task_result.resultJson;正文引用归 M07/M05)。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QaTaskResult {
    pub answer: AnswerContent,
    pub model_provider_id: String,
    pub model_id: String,
    pub retrieval_run_id: String,
    /// 本轮实际进入 Prompt 的历史消息条数。
    pub history_turns: usize,
    /// 历史裁剪说明(未裁剪为 None;不静默丢弃)。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_note: Option<String>,
    /// 本轮是否把历史摘要送入了模型(压缩历史不是原文来源)。
    pub summary_used: bool,
    /// 本轮上下文说明:摘要使用/失败、历史与检索裁剪原因。
    /// 只在确有需要时出现,不包含正文、密钥或完整路径。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_note: Option<String>,
}

/// 请求(M13 组装;`ai_ask` 命令层反序列化)。
pub struct AskRequest {
    pub document_id: String,
    pub conversation_id: String,
    /// M07 助手占位消息 ID:提供时 Citation 属主按消息粒度校验
    /// (message:{id});缺省回退 conversation 粒度(测试替身场景)。
    pub assistant_message_id: Option<String>,
    pub snapshot_id: String,
    pub question: String,
    pub locale: String,
    pub idempotency_key: String,
    /// 最近对话轮次(按时间升序;不含当前问题)。
    pub history: Vec<ChatTurn>,
    /// 本轮随问题发送的图片附件(M13 从附件服务读取并校验)。
    pub attachments: Vec<super::models::GenerationAttachment>,
}

/// 已创建任务、等待执行的一次问答(流式命令在后台线程消费)。
pub struct PreparedAsk {
    pub task: TaskRow,
    pub snapshot: SnapshotContext,
    pub request: AskRequest,
}

pub struct RetrievalAiService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    /// 事件总线在 M13 接线;M09 阶段保持装配位(事件只在 COMMIT 后发)。
    #[allow(dead_code)]
    events: Arc<dyn EventSink>,
    tasks: Arc<TaskService>,
    parse: Arc<ParseIndexService>,
    /// M07 消息落地前的装配位:届时在同事务内写入助手消息属主。
    #[allow(dead_code)]
    evidence: Arc<EvidenceService>,
    /// Citation 属主校验器(M07 消息注册;测试注入)。
    citation_owner_checks: Arc<OwnerValidators>,
    snapshots: Arc<dyn ContextSnapshotPort>,
    generation: Arc<dyn StructuredGenerationPort>,
    /// M07 滚动摘要(未接线时 None:仅使用最近原文)。
    summaries: Option<Arc<dyn ConversationSummaryPort>>,
    config: RetrievalConfig,
}

impl RetrievalAiService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
        tasks: Arc<TaskService>,
        parse: Arc<ParseIndexService>,
        evidence: Arc<EvidenceService>,
        snapshots: Arc<dyn ContextSnapshotPort>,
        generation: Arc<dyn StructuredGenerationPort>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
            tasks,
            parse,
            evidence,
            citation_owner_checks: Arc::new(OwnerValidators::default()),
            snapshots,
            generation,
            summaries: None,
            config: DEFAULT_CONFIG,
        }
    }

    /// 接线 M07 滚动摘要:生成仍走同一 EgressGateway,不新增出口。
    pub fn with_conversation_summary_port(
        mut self,
        port: Arc<dyn ConversationSummaryPort>,
    ) -> Self {
        self.summaries = Some(port);
        self
    }

    /// M07 在消息创建时注册属主校验器(同一事务可见未提交行)。
    pub fn register_citation_owner_check(&self, check: OwnerCheckFn) {
        // OwnerValidators::register 需要可变引用;此处用内部可变性不足,
        // 由调用方在组装时整体传入,运行期只读。
        let _ = check;
    }

    pub fn with_citation_owner_validators(mut self, validators: Arc<OwnerValidators>) -> Self {
        self.citation_owner_checks = validators;
        self
    }

    pub fn config(&self) -> &RetrievalConfig {
        &self.config
    }

    /// 命令层编排(M07 完成助手消息)读取已保存答案;无结果返回 None。
    pub fn task_answer(&self, task_id: &str) -> Result<Option<AnswerContent>, AppError> {
        let tid = task_id.to_string();
        let json: Option<String> = self.db.read(move |conn| {
            use rusqlite::OptionalExtension;
            conn.query_row(
                "SELECT result_json FROM task_result WHERE task_id=?1",
                [&tid],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| crate::persistence::db_app_error(&e))
        })?;
        let Some(json) = json else { return Ok(None) };
        let value: serde_json::Value =
            serde_json::from_str(&json).map_err(|e| AppError::internal(e.to_string()))?;
        let answer = serde_json::from_value(value["answer"].clone())
            .map_err(|e| AppError::internal(e.to_string()))?;
        Ok(Some(answer))
    }

    /// 命令层反查:快照所属文档(不加载完整上下文)。
    pub fn snapshot_document_id(&self, snapshot_id: &str) -> Result<Option<String>, AppError> {
        self.snapshots
            .load(snapshot_id)
            .map(|s| Some(s.document_id))
    }

    /// `ai_ask` 主入口:同步执行整个问答编排。幂等:同 idempotencyKey
    /// 的重复请求返回已有 Task(M09-A7)。
    pub fn ask(&self, req: AskRequest) -> Result<TaskRow, AppError> {
        self.ask_with_observer(req, noop_observer())
    }

    /// 流式入口:同一编排,额外把真实阶段与 provider reasoning 转发给观察者。
    /// 流事件只用于临时展示;最终答案仍走校验与事务提交。
    pub fn ask_streamed(
        &self,
        req: AskRequest,
        observer: SharedObserver,
    ) -> Result<TaskRow, AppError> {
        self.ask_with_observer(req, observer)
    }

    fn ask_with_observer(
        &self,
        req: AskRequest,
        observer: SharedObserver,
    ) -> Result<TaskRow, AppError> {
        let (task, prepared) = self.prepare_ask(req)?;
        match prepared {
            Some(prepared) => self.execute_prepared(prepared, observer),
            // 同一幂等键的重复发送:返回已有 Task,不重复消息与费用。
            None => Ok(task),
        }
    }

    /// 任务/快照前置校验与创建(不执行检索与模型)。
    /// 供流式命令立即返回 TaskView,随后在后台执行 `execute_prepared`。
    pub fn prepare_ask(&self, req: AskRequest) -> Result<(TaskRow, Option<PreparedAsk>), AppError> {
        if req.question.trim().is_empty() {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "empty_question".into()));
        }
        // 快照存在性先于任务创建:无效快照不产生任务。
        let snapshot = self.snapshots.load(&req.snapshot_id)?;
        if snapshot.document_id != req.document_id {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "snapshot_document_mismatch".into()));
        }
        let task = self.tasks.create_task(CreateTask {
            document_id: Some(req.document_id.clone()),
            task_type: TaskType::AskQuestion,
            idempotency_key: req.idempotency_key.clone(),
            phase: TaskPhase::Waiting,
        })?;
        if matches!(
            task.status,
            TaskStatus::Succeeded
                | TaskStatus::Failed
                | TaskStatus::Canceled
                | TaskStatus::Interrupted
        ) {
            return Ok((task, None));
        }
        Ok((
            task.clone(),
            Some(PreparedAsk {
                task,
                snapshot,
                request: req,
            }),
        ))
    }

    /// 执行已创建任务的一次 Attempt;失败同时把任务置为失败终态。
    pub fn execute_prepared(
        &self,
        prepared: PreparedAsk,
        observer: SharedObserver,
    ) -> Result<TaskRow, AppError> {
        match self.run_attempt(
            &prepared.task,
            &prepared.snapshot,
            &prepared.request,
            &observer,
        ) {
            Ok(t) => Ok(t),
            Err((task_err, attempt)) => {
                if let Some(attempt) = attempt {
                    // 取消必须落 canceled(而不是 failed):任务中心把 failed
                    // 归为"失败"并显示错误码,与用户主动取消的语义不符。
                    // settle_generation_failure 在 TASK_CANCELED 时走 cancel_ack,
                    // 且只有在确实收到取消请求时才允许 ack。
                    let _ = self.tasks.settle_generation_failure(
                        &prepared.task.id,
                        &attempt.id,
                        &task_err.code,
                    );
                }
                Err(task_err)
            }
        }
    }

    /// `ai_retry`:同一 Task 新 Attempt,不重复用户消息(A7)。
    /// `question` 由调用方(M07 会话层)携带原问题;Manifest 只保存查询哈希。
    pub fn retry(&self, task_id: &str, question: Option<String>) -> Result<TaskRow, AppError> {
        let task = self.tasks.retry_task(task_id)?;
        // 快照从上一次运行的 retrieval_run 恢复(问题正文不入 Manifest;
        // 由调用方 M07 消息提供;此处读 run 记录的快照 ID)。
        let snapshot_id: Option<String> = self.db.read(|conn| {
            conn.query_row(
                "SELECT snapshot_id FROM retrieval_run WHERE task_id=?1",
                [task_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| crate::persistence::db_app_error(&e))
        })?;
        let Some(snapshot_id) = snapshot_id else {
            return Err(AppError::new(
                CoreErrorCode::NotFound,
                crate::shared::new_trace_id(),
            ));
        };
        let snapshot = self.snapshots.load(&snapshot_id)?;
        let req = AskRequest {
            document_id: snapshot.document_id.clone(),
            conversation_id: String::new(),
            assistant_message_id: None,
            snapshot_id: snapshot.snapshot_id.clone(),
            // 重试必须由调用方(M07 会话层)携带原问题:Manifest 只保存
            // 查询哈希,问题正文不入普通日志,无法从本模块还原。
            question: question.unwrap_or_default(),
            locale: "zh-CN".into(),
            idempotency_key: format!("retry:{}", task.id),
            history: Vec::new(),
            attachments: Vec::new(),
        };
        if req.question.trim().is_empty() {
            return Err(AppError::new(
                CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "retry_requires_question".into()));
        }
        match self.run_attempt(&task, &snapshot, &req, &noop_observer()) {
            Ok(t) => Ok(t),
            Err((task_err, attempt)) => {
                if let Some(attempt) = attempt {
                    let _ = self.tasks.fail_task(&task.id, &attempt.id, &task_err.code);
                }
                Err(task_err)
            }
        }
    }

    /// 单次 Attempt 的完整编排。错误返回 (对外错误, 已建 Attempt)。
    fn run_attempt(
        &self,
        task: &TaskRow,
        snapshot: &SnapshotContext,
        req: &AskRequest,
        observer: &SharedObserver,
    ) -> Result<TaskRow, (AppError, Option<AttemptRow>)> {
        let cap = self
            .generation
            .capability(&req.document_id)
            .map_err(fail_early)?;
        let (task, attempt) = self
            .tasks
            .begin_attempt(
                task.id.as_str(),
                cap.provider_id.clone(),
                cap.model_id.clone(),
            )
            .map_err(|e| (e, None))?;

        // 覆盖只约束文档问题;一般问题仍须通过模型能力与隐私检查。
        let document_question = requires_document_context(&req.question, &req.history);
        let mode = self
            .precheck(snapshot, &cap, document_question)
            .map_err(|e| (e, Some(attempt.clone())))?;
        if document_question && mode == CapabilityMode::Unavailable {
            return Err((m09err::context_coverage_incomplete(), Some(attempt)));
        }

        // 取消令牌:请求取消后停止检索与模型流(§9)。
        let cancel = self.tasks.cancel_token(&task.id);
        if cancel.is_cancelled() {
            return Err((
                AppError::new(CoreErrorCode::TaskCanceled, crate::shared::new_trace_id()),
                Some(attempt),
            ));
        }

        observer.on_stage(StreamStage::Preparing);
        let _ = self
            .tasks
            .set_phase(&task.id, &attempt.id, TaskPhase::Retrieving)
            .map_err(|e| (e, Some(attempt.clone())))?;
        observer.on_stage(StreamStage::Retrieving);

        // 多轮查询改写:短追问/指代问题结合上一轮问题扩检索词。
        // 只扩检索词与向量文本,不改变答案问题,也不扩大快照授权范围。
        let query = parse_query_with_previous(&req.question, last_user_question(&req.history));

        // —— 检索 + 融合(A1/A2/A9)——
        let mut retrieval = self
            .retrieve(&task, snapshot, &query, &mode)
            .map_err(|e| (e, Some(attempt.clone())))?;

        // 文档问题检索零命中时,在**已冻结的范围白名单内**回填背景块。
        //
        // 两种真实场景:(1) 概括性问题(如“总结一下”)没有可匹配的关键词;
        // (2) 混合问题(如“你好,结合本文总结一下”)的关键词含寒暄词,
        // FTS/子串匹配不到任何块。此时直接判“证据不足”会把用户明明
        // 选中的范围当成没有正文。
        //
        // 安全边界不变:回填只从该快照自己的块集合取,不扩大到范围外,
        // 也不会让模型引用它没看过的块;作者/页码等定位信息仍来自原文块。
        if document_question
            && retrieval.included_blocks.is_empty()
            && snapshot.note_sources.is_empty()
            && req.attachments.is_empty()
        {
            if let Some(fallback_blocks) = self.scope_backfill_blocks(snapshot) {
                if !fallback_blocks.is_empty() {
                    let fallback_decisions: Vec<CandidateDecision> = fallback_blocks
                        .iter()
                        .map(|b| CandidateDecision {
                            candidate: FusedCandidate {
                                block_id: b.id.clone(),
                                rrf_score: 1.0,
                                fts_rank: None,
                                vector_rank: None,
                            },
                            included: true,
                            exclusion_reason: None,
                        })
                        .collect();
                    let _ = self.persist_candidates(&retrieval.run_id, &fallback_decisions);
                    retrieval.decisions = fallback_decisions;
                    retrieval.included_blocks = fallback_blocks;
                }
            }
        }

        // 检索不到原文块时不再提前返回 insufficient_evidence:
        // 问候、自我介绍、能力边界与一般常识问题必须由模型真实回答;
        // 文档事实问题在无来源时由模型如实说明,不得伪造引用。
        // 只有模型不可用/超时/输出完全不可恢复时才走可见兜底。

        // —— 滚动摘要(WBS-5):模型调用复用同一 Task/Attempt 与许可链 ——
        // 摘要在检索之后、主生成之前刷新;失败只记录,不阻塞主问答。
        let summary = self
            .refresh_conversation_summary(&task, &attempt, snapshot, req, &cancel)
            .map_err(|e| (e, Some(attempt.clone())))?;
        if cancel.is_cancelled() {
            return Err((
                AppError::new(CoreErrorCode::TaskCanceled, crate::shared::new_trace_id()),
                Some(attempt),
            ));
        }
        let mut context_notes = summary.notes.clone();

        // —— 分层上下文:摘要 + 最近原文 + 当前问题(按真实窗口预算) ——
        // 2F:按模型能力表的上下文窗口计算,不再固定 16k;未知时回退默认。
        // 预算按真实窗口计算,但这是内部实现细节:不展示给用户。
        let token_budget = self.token_budget_for_model();
        let layered = budget::fit_history_and_question(
            summary.text.as_deref(),
            &req.history,
            &query.normalized_question,
            token_budget.history,
        );
        // 只有真正装入 Prompt 的摘要才算"已结合";超预算被省略时
        // layered.notes 会给出可见原因。
        let summary_used = layered.summary.is_some();
        if summary_used {
            context_notes.push("已结合历史摘要".to_string());
        }
        context_notes.extend(layered.notes.iter().cloned());

        let system_tokens = estimate_tokens(system_instruction_for(super::models::TaskKind::Qa));
        let system_overflow = system_tokens.saturating_sub(token_budget.system);
        if system_overflow > 0 {
            context_notes.push("系统指令超出预留预算,已压缩检索来源".to_string());
        }
        let retrieval_budget = token_budget.retrieval.saturating_sub(system_overflow);

        // —— Prompt 装填(§6):按剩余 token 预算取舍并记录排除原因(A9) ——
        let mut payload = assemble_prompt_with_token_budget(
            PromptAssembly {
                blocks: retrieval.included_blocks.clone(),
                extra_sources: if document_question {
                    snapshot.note_sources.clone()
                } else {
                    Vec::new()
                },
            },
            MAX_PROMPT_BLOCKS,
            retrieval_budget,
        );
        // 装填裁剪原因:只保留用户能理解的部分,过滤纯预算/块数内部细节。
        context_notes.extend(payload.notes.iter().filter(|n| !is_budget_note(n)).cloned());

        // 以真正进入 Prompt 的来源为准收缩引用白名单:被 token 预算排除的块
        // 不能作为本轮 citation 的来源,否则模型引用了它没看过的内容。
        let prompt_ids: HashSet<String> = payload
            .inputs
            .iter()
            .filter(|input| input.role == super::models::SourceRole::Original)
            .map(|input| input.source_id.clone())
            .collect();
        let reason_by_id: std::collections::HashMap<&str, &'static str> = payload
            .exclusions
            .iter()
            .map(|(id, reason)| (id.as_str(), *reason))
            .collect();
        let dropped: Vec<(String, &'static str)> = retrieval
            .included_blocks
            .iter()
            .filter(|block| !prompt_ids.contains(&block.id))
            .map(|block| {
                (
                    block.id.clone(),
                    reason_by_id
                        .get(block.id.as_str())
                        .copied()
                        .unwrap_or("prompt_token_budget_exceeded"),
                )
            })
            .collect();
        if !dropped.is_empty() {
            self.mark_candidates_excluded(&retrieval.run_id, &dropped)
                .map_err(|e| (e, Some(attempt.clone())))?;
            retrieval
                .included_blocks
                .retain(|block| prompt_ids.contains(&block.id));
        }

        // 问候/自我介绍/能力边界:不附带原文来源,避免模型把寒暄答成资料复述。
        let conversational = is_conversational_question(&query.normalized_question);
        if conversational {
            payload.inputs.clear();
            payload.tokens = 0;
        }

        if payload.inputs.is_empty()
            && req.attachments.is_empty()
            && !retrieval.included_blocks.is_empty()
            && !conversational
        {
            return Err((m09err::context_budget_exceeded(), Some(attempt)));
        }

        let _ = self
            .tasks
            .set_phase(&task.id, &attempt.id, TaskPhase::Generating)
            .map_err(|e| (e, Some(attempt.clone())))?;
        observer.on_stage(StreamStage::Generating);

        // —— 文档地图(2F):标题树 + 页范围,导航用、不可引用 ——
        let document_map = if document_question {
            self.document_map_for(snapshot)
        } else {
            None
        };
        if document_map.is_some() {
            context_notes.push("已附文档地图(章节导航,不是引用来源)".to_string());
        }

        // —— 结构化生成(M08 Port;§8)——
        let request = build_qa_request(
            &task.id,
            &attempt.id,
            &req.document_id,
            &snapshot.snapshot_id,
            answer_json_schema(),
            &layered.question,
            &req.locale,
            payload.inputs.clone(),
            layered.turns.clone(),
            layered.summary.clone(),
            req.attachments.clone(),
            document_map,
        );
        let generated = self
            .generation
            .generate_streamed(&request, &cancel, observer)
            .map_err(|e| (m09err::map_egress_error(e), Some(attempt.clone())))?;

        let _ = self
            .tasks
            .set_phase(&task.id, &attempt.id, TaskPhase::Validating)
            .map_err(|e| (e, Some(attempt.clone())))?;
        observer.on_stage(StreamStage::Validating);

        // —— AnswerContent 校验(§7、A5/A6)——
        // 白名单以实际进入 Prompt 的来源为准(含笔记/草稿预算裁剪)。
        let answer = self
            .validate_answer(
                generated.content.clone(),
                &retrieval,
                &payload.inputs,
                &query,
            )
            .map_err(|e| (e, Some(attempt.clone())))?;

        let outcome = match answer.status {
            AnswerStatus::Answered => TaskOutcome::Answered,
            AnswerStatus::InsufficientEvidence => TaskOutcome::InsufficientEvidence,
            AnswerStatus::Conflict => TaskOutcome::Conflict,
            AnswerStatus::OutOfScope => TaskOutcome::OutOfScope,
        };
        observer.on_stage(StreamStage::Finalizing);
        self.commit_answer(
            &task,
            &attempt,
            &answer,
            &generated.model_provider_id,
            &generated.model_id,
            &retrieval,
            outcome,
            req.conversation_id.as_str(),
            req.assistant_message_id.as_deref(),
            layered.turns.len(),
            layered.history_note(),
            summary_used,
            join_notes(context_notes),
        )
        .map_err(|e| (e, Some(attempt)))
    }

    /// 滚动摘要刷新。成功时返回本轮可用的摘要;失败只记录状态并生成
    /// 用户可见说明,绝不阻塞主问答。取消属于任务控制流,会向上传播。
    fn refresh_conversation_summary(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        snapshot: &SnapshotContext,
        req: &AskRequest,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<SummaryRefresh, AppError> {
        let Some(port) = &self.summaries else {
            return Ok(SummaryRefresh::default());
        };
        if req.conversation_id.trim().is_empty() {
            return Ok(SummaryRefresh::default());
        }
        let plan = match port.plan(&req.conversation_id) {
            Ok(plan) => plan,
            Err(e) => {
                return Ok(SummaryRefresh {
                    text: None,
                    notes: vec![format!("历史摘要不可用({})", e.code)],
                });
            }
        };
        let text = plan
            .previous_summary
            .clone()
            .filter(|summary| !summary.trim().is_empty());
        let used = text.is_some();
        let mut notes = Vec::new();
        let previous_failed = plan.previous_status.as_deref() == Some("failed");

        if plan.messages.is_empty() {
            if previous_failed {
                notes.push(if used {
                    "上次会话摘要更新失败,已沿用旧摘要".to_string()
                } else {
                    "会话摘要暂不可用,本次仅使用最近对话".to_string()
                });
            }
            return Ok(SummaryRefresh { text, notes });
        }

        let Some(prepared) = super::history::prepare_summary_input(&plan) else {
            if previous_failed {
                notes.push(if used {
                    "上次会话摘要更新失败,已沿用旧摘要".to_string()
                } else {
                    "会话摘要暂不可用,本次仅使用最近对话".to_string()
                });
            }
            return Ok(SummaryRefresh { text, notes });
        };
        if prepared.previous_summary_truncated {
            notes.push("已有摘要过长,会话摘要输入已截断".to_string());
        }
        if prepared.truncated_turns > 0 {
            notes.push(format!(
                "{} 条历史消息过长,摘要输入已截断",
                prepared.truncated_turns
            ));
        }
        let remaining_unsummarized = plan
            .older_unsummarized_turns
            .saturating_sub(prepared.turns.len());
        if remaining_unsummarized > 0 {
            notes.push(format!(
                "还有 {} 条较早对话尚未纳入摘要,将在后续轮次继续压缩",
                remaining_unsummarized
            ));
        }

        let request = build_conversation_summary_request(
            &task.id,
            &attempt.id,
            &req.document_id,
            &snapshot.snapshot_id,
            &req.locale,
            &prepared,
        );
        let generated = match self.generation.generate(&request, cancel) {
            Ok(generated) => generated,
            Err(e) => {
                if e.code == "TASK_CANCELED" {
                    return Err(e);
                }
                let _ = port.save_failure(&req.conversation_id, &e.code);
                notes.push(summary_failure_note(&e.code, used));
                return Ok(SummaryRefresh { text, notes });
            }
        };
        let parsed = match parse_conversation_summary(
            &generated.content,
            &prepared.covered_up_to_message_id,
        ) {
            Ok(parsed) => parsed,
            Err(e) => {
                let _ = port.save_failure(&req.conversation_id, &e.code);
                notes.push(summary_failure_note(&e.code, used));
                return Ok(SummaryRefresh { text, notes });
            }
        };
        if parsed.truncated {
            notes.push("会话摘要超过长度上限,已截断保存".to_string());
        }
        if let Err(e) = port.save_summary(
            &req.conversation_id,
            &prepared.covered_up_to_message_id,
            &parsed.text,
            parsed.tokens,
        ) {
            notes.push(format!("会话摘要未能保存({})", e.code));
        }
        Ok(SummaryRefresh {
            text: Some(parsed.text),
            notes,
        })
    }

    /// 把真正进入 Prompt 前的排除原因写回检索候选(A9:无静默截断)。
    fn mark_candidates_excluded(
        &self,
        run_id: &str,
        exclusions: &[(String, &'static str)],
    ) -> Result<(), AppError> {
        if exclusions.is_empty() {
            return Ok(());
        }
        let run_id = run_id.to_string();
        let rows: Vec<(String, String)> = exclusions
            .iter()
            .map(|(id, reason)| (id.clone(), (*reason).to_string()))
            .collect();
        self.db.write(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            for (block_id, reason) in rows {
                tx.execute(
                    "UPDATE retrieval_candidate SET included_in_prompt=0, exclusion_reason=?3 \
                     WHERE run_id=?1 AND block_id=?2",
                    rusqlite::params![run_id, block_id, reason],
                )
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            }
            tx.commit()
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            Ok(())
        })
    }

    // —— 模型前检查(《M09》§3、A8)——

    fn precheck(
        &self,
        snapshot: &SnapshotContext,
        cap: &ModelCapabilityView,
        document_question: bool,
    ) -> Result<CapabilityMode, AppError> {
        // 授权与模型可用性对任何问法都不可绕过。
        if !cap.generation_authorized {
            return Err(m09err::privacy_grant_required());
        }
        if !cap.generation_available {
            return Err(m09err::model_unavailable());
        }
        if !document_question {
            // 无需文档检索不是模型不可用;后续仍走已有 generation gateway。
            return Ok(CapabilityMode::Unavailable);
        }
        let status = self.parse.get_status(&snapshot.document_id)?;
        Ok(resolve_capability_mode(
            &snapshot.scope_kind,
            snapshot.coverage_complete,
            status.capability,
            status.fts_state,
            status.vector_state,
        ))
    }

    // —— 检索(§5)——

    fn retrieve(
        &self,
        task: &TaskRow,
        snapshot: &SnapshotContext,
        query: &ParsedQuery,
        mode: &CapabilityMode,
    ) -> Result<RetrievalOutcome, AppError> {
        if *mode == CapabilityMode::Unavailable
            || (query.keywords.is_empty() && snapshot.explicit_block_ids.is_empty())
        {
            // 无来源仍记录 Manifest,主生成不早退。
            let run_id = self.persist_run(task, snapshot, query, mode, false, &[])?;
            return Ok(RetrievalOutcome {
                run_id,
                included_blocks: Vec::new(),
                decisions: Vec::new(),
                capability_mode: *mode,
                degraded_path: false,
                snapshot_id: snapshot.snapshot_id.clone(),
                document_version_id: snapshot.document_version_id.clone(),
            });
        }

        let mut lists: Vec<RankedList> = Vec::new();
        let mut degraded = false;

        match (mode, &snapshot.scope_kind) {
            (CapabilityMode::ExplicitBlocks, SnapshotScopeKind::ExplicitBlocks) => {
                // 明确选区:按原文顺序直接装填,不调用向量召回(§5)。
            }
            (m, _) => {
                // 范围内 FTS:过滤先于排名 —— 显式块集合限定下执行检索,
                // 禁止全文 TopK 后过滤(A2)。
                if !query.keywords.is_empty() {
                    let (ranked, used_like) = self.scope_fts(snapshot, query, m)?;
                    degraded = used_like;
                    if !ranked.is_empty() {
                        lists.push(RankedList {
                            source: "fts",
                            entries: ranked,
                            weight: 1.0,
                        });
                    }
                }
                // 向量:仅完整混合模式 + 生成代次可用时;范围内精确距离。
                if *m == CapabilityMode::Hybrid {
                    if let Some(query_vec) = self.generation.query_embedding(
                        &query.embedding_text,
                        &snapshot.parse_generation_id,
                        &snapshot.document_id,
                    ) {
                        let ranked = self.scope_vector(snapshot, &query_vec)?;
                        if !ranked.is_empty() {
                            lists.push(RankedList {
                                source: "vec",
                                entries: ranked,
                                weight: 1.0,
                            });
                        }
                    }
                }
            }
        }

        let fused = rrf_fuse(&lists, &self.config);
        // 快照白名单:显式块集合;全文快照 = 代次全部块(闭包恒 true,
        // 候选本身来自代次内检索)。
        let explicit: HashSet<String> = snapshot.explicit_block_ids.iter().cloned().collect();
        let is_allowed = |id: &str| explicit.is_empty() || explicit.contains(id);
        // 长上下文:不再把融合候选硬截断到 24 块。保留冻结的排序配置,
        // 但允许候选集合扩到安全上限,由 Prompt 的 token 预算迭代取舍。
        let candidate_config = RetrievalConfig {
            fused_candidate_limit: self.config.fused_candidate_limit.max(MAX_PROMPT_BLOCKS),
            ..self.config.clone()
        };
        let decisions = decide_candidates(fused, &is_allowed, MAX_PROMPT_BLOCKS, &candidate_config);

        // 装填候选块(按原文顺序);explicit 模式直接按显式集合装填。
        let (decisions, included_ids): (Vec<CandidateDecision>, Vec<String>) =
            if snapshot.scope_kind == SnapshotScopeKind::ExplicitBlocks && lists.is_empty() {
                // 明确选区直接装填:块集合本身就是候选清单(无排名,§5)。
                let ds = snapshot
                    .explicit_block_ids
                    .iter()
                    .map(|id| CandidateDecision {
                        candidate: FusedCandidate {
                            block_id: id.clone(),
                            rrf_score: 0.0,
                            fts_rank: None,
                            vector_rank: None,
                        },
                        included: true,
                        exclusion_reason: None,
                    })
                    .collect();
                let ids = snapshot.explicit_block_ids.clone();
                (ds, ids)
            } else {
                let ids = decisions
                    .iter()
                    .filter(|d| d.included)
                    .map(|d| d.candidate.block_id.clone())
                    .collect();
                (decisions, ids)
            };
        let mut blocks = self.parse.get_blocks_by_ids(&included_ids)?;
        blocks.sort_by_key(|b| b.ordinal.unwrap_or(i64::MAX));

        let run_id = self.persist_run(task, snapshot, query, mode, degraded, &decisions)?;
        // 候选持久化(含排除原因,A9)。
        self.persist_candidates(&run_id, &decisions)?;

        Ok(RetrievalOutcome {
            run_id,
            included_blocks: blocks,
            decisions,
            capability_mode: *mode,
            degraded_path: degraded,
            snapshot_id: snapshot.snapshot_id.clone(),
            document_version_id: snapshot.document_version_id.clone(),
        })
    }

    /// 检索零命中时按快照范围回填块(只在快照自己的白名单内,不扩大范围)。
    /// 全文快照按代次取最前若干块;局部/显式范围只取该快照冻结的块。
    fn scope_backfill_blocks(&self, snapshot: &SnapshotContext) -> Option<Vec<ContentBlockView>> {
        const BACKFILL_LIMIT: usize = 24;
        let mut blocks = if snapshot.explicit_block_ids.is_empty() {
            let gen_id = snapshot.parse_generation_id.as_ref()?;
            self.parse.list_generation_blocks(gen_id).ok()?
        } else {
            self.parse
                .get_blocks_by_ids(&snapshot.explicit_block_ids)
                .ok()?
        };
        blocks.retain(|block| !block.text.trim().is_empty());
        blocks.sort_by_key(|block| block.ordinal.unwrap_or(i64::MAX));
        blocks.truncate(BACKFILL_LIMIT);
        Some(blocks)
    }

    /// 范围内 FTS:显式块集合 → 逐块子串检查;全文/无显式集合 →
    /// M04 代次内 fts_search。返回 (ranked, used_like)。
    fn scope_fts(
        &self,
        snapshot: &SnapshotContext,
        query: &ParsedQuery,
        mode: &CapabilityMode,
    ) -> Result<(Vec<(String, usize)>, bool), AppError> {
        let use_like = query
            .keywords
            .iter()
            .any(|k| k.chars().count() < crate::parse_index::fts::TRIGRAM_MIN_CHARS);
        if !snapshot.explicit_block_ids.is_empty() {
            // 局部范围:在限定块集合上做有界子串检查(过滤先于排名)。
            let blocks = self.parse.get_blocks_by_ids(&snapshot.explicit_block_ids)?;
            let mut hits: Vec<(String, usize)> = Vec::new();
            for block in &blocks {
                let matched = query.keywords.iter().any(|kw| {
                    block.text.contains(kw.as_str())
                        || block.text.to_lowercase().contains(&kw.to_lowercase())
                });
                if matched {
                    hits.push((block.id.clone(), 0));
                }
            }
            let ranked = hits
                .into_iter()
                .enumerate()
                .map(|(i, (id, _))| (id, i + 1))
                .collect();
            return Ok((ranked, use_like));
        }
        // 全文:代次内 FTS(候选上限内)。仅 Hybrid/FtsOnly 有代次。
        if !matches!(mode, CapabilityMode::Hybrid | CapabilityMode::FtsOnly) {
            return Ok((Vec::new(), use_like));
        }
        let gen_id = snapshot
            .parse_generation_id
            .as_deref()
            .ok_or_else(m09err::context_coverage_incomplete)?;
        let hits = self.parse.fts_search(
            gen_id,
            &query.keywords,
            self.config.fts_candidate_limit as i64,
        )?;
        Ok((
            hits.into_iter()
                .enumerate()
                .map(|(i, h)| (h.block_id, i + 1))
                .collect(),
            use_like,
        ))
    }

    /// 范围内向量:显式块集合 → vector_search_within(精确距离);
    /// 全文 → vector_search_full(代次专属)。
    fn scope_vector(
        &self,
        snapshot: &SnapshotContext,
        query_vec: &[f32],
    ) -> Result<Vec<(String, usize)>, AppError> {
        let hits: Vec<VectorHit> = if !snapshot.explicit_block_ids.is_empty() {
            self.parse.vector_search_within(
                &snapshot.explicit_block_ids,
                query_vec,
                self.config.vector_candidate_limit as i64,
            )?
        } else {
            let gen_id = snapshot
                .parse_generation_id
                .as_deref()
                .ok_or_else(m09err::context_coverage_incomplete)?;
            self.parse.vector_search_full(
                gen_id,
                query_vec,
                self.config.vector_candidate_limit as i64,
            )?
        };
        Ok(hits
            .into_iter()
            .enumerate()
            .map(|(i, h)| (h.block_id, i + 1))
            .collect())
    }

    // —— Manifest 持久化 ——

    /// 2F 文档地图:仅对全文/多页范围有意义(局部范围已有明确上下文)。
    /// 地图只进 system 层,不进引用白名单。
    fn document_map_for(&self, snapshot: &SnapshotContext) -> Option<String> {
        if snapshot.scope_kind == SnapshotScopeKind::ExplicitBlocks {
            return None;
        }
        let generation_id = snapshot.parse_generation_id.as_deref()?;
        let blocks = self.parse.list_generation_blocks(generation_id).ok()?;
        super::document_map::document_map_for(&blocks).map(|(map, _)| map)
    }

    /// 按当前模型真实窗口计算预算。
    /// 预算数值是内部实现细节,不进入用户可见文案。
    fn token_budget_for_model(&self) -> TokenBudget {
        match self.generation.context_window() {
            Some(window) if window > 0 => TokenBudget::for_context_window(window),
            _ => TokenBudget::default_budget(),
        }
    }

    fn persist_run(
        &self,
        task: &TaskRow,
        snapshot: &SnapshotContext,
        query: &ParsedQuery,
        mode: &CapabilityMode,
        degraded: bool,
        decisions: &[CandidateDecision],
    ) -> Result<String, AppError> {
        let run_id = self.ids.new_entity_id();
        let included = decisions.iter().filter(|d| d.included).count();
        let token_budget = TokenBudget::default_budget();
        let config_json = serde_json::json!({
            "searchConfigVersion": self.config.search_config_version,
            "ftsCandidateLimit": self.config.fts_candidate_limit,
            "vectorCandidateLimit": self.config.vector_candidate_limit,
            "fusedCandidateLimit": self.config.fused_candidate_limit,
            "candidateDecisionLimit": self.config.fused_candidate_limit.max(MAX_PROMPT_BLOCKS),
            "rrfK": self.config.rrf_k,
            "promptBlockBudget": MAX_PROMPT_BLOCKS,
            "promptCandidateLimit": MAX_PROMPT_BLOCKS,
            "promptTokenBudget": token_budget.retrieval,
            "historyTokenBudget": token_budget.history,
            "queryRewrite": query.rewritten_from_previous,
            "includedInPrompt": included,
            "retrievalDurationMs": null,
        });
        let now = self.clock.now_ms();
        let run_id2 = run_id.clone();
        let task_id = task.id.clone();
        let snapshot_id = snapshot.snapshot_id.clone();
        let query_hash = sha256_str(&query.normalized_question);
        let mode_s = mode.as_str();
        let config_s = config_json.to_string();
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO retrieval_run(id, task_id, snapshot_id, query_text_hash, \
                 config_json, capability_mode, degraded_path, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                rusqlite::params![
                    run_id2,
                    task_id,
                    snapshot_id,
                    query_hash,
                    config_s,
                    mode_s,
                    degraded as i64,
                    now
                ],
            )
            .map_err(|e| crate::persistence::db_app_error(&e))?;
            Ok(())
        })?;
        Ok(run_id)
    }

    fn persist_candidates(
        &self,
        run_id: &str,
        decisions: &[CandidateDecision],
    ) -> Result<(), AppError> {
        if decisions.is_empty() {
            return Ok(());
        }
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, Option<i64>, Option<i64>, f64, i64, Option<String>)> = decisions
            .iter()
            .map(|d| {
                (
                    d.candidate.block_id.clone(),
                    d.candidate.fts_rank.map(|r| r as i64),
                    d.candidate.vector_rank.map(|r| r as i64),
                    d.candidate.rrf_score,
                    d.included as i64,
                    d.exclusion_reason.map(|r| r.as_str().to_string()),
                )
            })
            .collect();
        let run_id = run_id.to_string();
        self.db.write(move |conn| {
            let tx = conn
                .transaction()
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            for (block_id, fts_rank, vector_rank, score, included, reason) in rows {
                tx.execute(
                    "INSERT OR REPLACE INTO retrieval_candidate(run_id, block_id, fts_rank, \
                     vector_rank, rrf_score, included_in_prompt, exclusion_reason) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![
                        run_id,
                        block_id,
                        fts_rank,
                        vector_rank,
                        score,
                        included,
                        reason
                    ],
                )
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            }
            tx.commit()
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            Ok(())
        })
    }

    // —— AnswerContent 校验(§7)——

    /// 语义级校验:引用分级(剔除/降级)而不是整轮拒绝。
    ///
    /// - 伪造/越界/引用资料 blockId:剔除该引用,不回退整条回答;
    /// - 引文不逐字:剔除该引用,受影响的 document_fact 降级为 general_note;
    /// - 合法引用保留;绝不把未核实内容标成已验证。
    fn validate_answer(
        &self,
        raw: serde_json::Value,
        _retrieval: &RetrievalOutcome,
        prompt_inputs: &[GenerationInput],
        query: &ParsedQuery,
    ) -> Result<AnswerContent, AppError> {
        // 模型未按 schema 输出但保留了正文:保留可读内容,不伪造引用。
        if raw.get("schemaVersion").and_then(|v| v.as_str()) == Some("plain_text_fallback.v1") {
            let text = raw.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let text = crate::shared::sanitize::salvage_readable_markdown(text);
            if text.trim().is_empty() {
                return Err(err_answer("plain_text_fallback_empty"));
            }
            return Ok(AnswerContent {
                schema_version: ANSWER_SCHEMA_VERSION.to_string(),
                status: AnswerStatus::Answered,
                segments: crate::retrieval_ai::models::plain_text_segments(&text),
                citations: Vec::new(),
                conflicts: Vec::new(),
                suggestions: Vec::new(),
                unverified_notes: Vec::new(),
            });
        }
        let mut answer = parse_answer_content(&raw)?;
        for segment in &mut answer.segments {
            segment.text = crate::shared::sanitize::sanitize_generated_markdown(&segment.text);
        }
        answer
            .segments
            .retain(|segment| !segment.text.trim().is_empty());
        if answer.segments.is_empty() {
            return Err(err_answer("answer_body_unrecoverable"));
        }
        // 问候/自我介绍/能力边界:模型误判为证据不足但已有正文时按 answered 返回。
        if is_conversational_question(&query.normalized_question)
            && answer.status != AnswerStatus::Answered
            && !answer.segments.is_empty()
        {
            answer.status = AnswerStatus::Answered;
        }

        let original_sources: HashSet<&str> = prompt_inputs
            .iter()
            .filter(|input| input.role == SourceRole::Original)
            .map(|input| input.source_id.as_str())
            .collect();
        let note_sources: HashSet<&str> = prompt_inputs
            .iter()
            .filter(|input| input.role != SourceRole::Original)
            .map(|input| input.source_id.as_str())
            .collect();
        let block_text = |id: &str| -> Option<String> {
            prompt_inputs
                .iter()
                .find(|input| input.source_id == id)
                .map(|input| input.text.clone())
        };

        // 非 answered 状态保留可见说明,但不得冒充原文事实或引用。
        if answer.status != AnswerStatus::Answered {
            for segment in &mut answer.segments {
                if segment.kind == SegmentKind::DocumentFact {
                    segment.kind = SegmentKind::GeneralNote;
                }
                segment.verification_status = "unverified".into();
                segment.citation_ids.clear();
            }
            answer.citations.clear();
            answer.conflicts.clear();
            return Ok(answer);
        }

        let mut dropped_citations = 0usize;
        let mut valid_citation_ids: HashSet<String> = HashSet::new();
        let mut kept: Vec<AnswerCitation> = Vec::with_capacity(answer.citations.len());
        for c in answer.citations.drain(..) {
            if c.block_id.trim().is_empty() || c.quoted_text.trim().is_empty() {
                dropped_citations += 1;
                continue;
            }
            // 引用资料(笔记/AI 草稿)不是 ContentBlock;越界/伪造 blockId 同样不落库。
            if note_sources.contains(c.block_id.as_str())
                || !original_sources.contains(c.block_id.as_str())
            {
                dropped_citations += 1;
                continue;
            }
            let Some(text) = block_text(&c.block_id) else {
                dropped_citations += 1;
                continue;
            };
            let norm_block = crate::shared::normalize_v1(&text).normalized;
            let norm_quote = crate::shared::normalize_v1(&c.quoted_text).normalized;
            if text.contains(&c.quoted_text) || norm_block.contains(&norm_quote) {
                valid_citation_ids.insert(c.id.clone());
                kept.push(c);
            } else {
                dropped_citations += 1;
            }
        }
        answer.citations = kept;

        // 段落侧同步:失效引用 id 移除;document_fact 失去全部引用后
        // 降级为 general_note(不冒充原文事实)。
        let mut downgraded_facts = 0usize;
        for segment in &mut answer.segments {
            segment
                .citation_ids
                .retain(|id| valid_citation_ids.contains(id));
            if segment.kind == SegmentKind::DocumentFact && segment.citation_ids.is_empty() {
                segment.kind = SegmentKind::GeneralNote;
                segment.verification_status = "unverified".into();
                downgraded_facts += 1;
            } else if segment.kind != SegmentKind::DocumentFact
                && segment.verification_status == "structurally_valid"
            {
                // 推断/说明段不得宣称已由原文核实。
                segment.verification_status = "unverified".into();
            }
        }
        for conflict in &mut answer.conflicts {
            conflict
                .citation_ids
                .retain(|id| valid_citation_ids.contains(id));
        }
        // 冲突条目失去全部引用后不再作为“有据冲突”展示。
        answer
            .conflicts
            .retain(|conflict| !conflict.citation_ids.is_empty());
        if dropped_citations > 0 || downgraded_facts > 0 {
            let mut note = String::from("部分引用未通过核对");
            if dropped_citations > 0 {
                note.push_str(&format!(",已剔除 {dropped_citations} 条"));
            }
            if downgraded_facts > 0 {
                note.push_str(&format!(",{downgraded_facts} 段已标为说明"));
            }
            answer.unverified_notes.push(note);
        }
        Ok(answer)
    }

    // —— 提交(§2:同一事务提交消息引用、引用和 TaskResult)——

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn commit_answer(
        &self,
        task: &TaskRow,
        attempt: &AttemptRow,
        answer: &AnswerContent,
        provider_id: &str,
        model_id: &str,
        retrieval: &RetrievalOutcome,
        outcome: TaskOutcome,
        conversation_id: &str,
        task_assistant_message_id: Option<&str>,
        history_turns: usize,
        history_note: Option<String>,
        summary_used: bool,
        context_note: Option<String>,
    ) -> Result<TaskRow, AppError> {
        // 先把引用 ID 换成落库 ID:模型写的 c1/c2 与 M05 表里的真实 ID
        // 不一致时,前端点击引用永远“无法定位该引用”。
        let citation_inputs: Vec<CitationInput> = answer
            .citations
            .iter()
            .map(|c| AnswerCitationInput::from_answer(c, answer))
            .collect();
        let citation_ids_seed: Vec<String> = (0..citation_inputs.len())
            .map(|_| self.ids.new_entity_id())
            .collect();
        let id_mapping: Vec<(String, String)> = answer
            .citations
            .iter()
            .zip(citation_ids_seed.iter())
            .map(|(citation, new_id)| (citation.id.clone(), new_id.clone()))
            .collect();
        let mut stored_answer = answer.clone();
        crate::retrieval_ai::models::rewrite_citation_ids(&mut stored_answer, &id_mapping);

        let result = QaTaskResult {
            answer: stored_answer,
            model_provider_id: provider_id.to_string(),
            model_id: model_id.to_string(),
            retrieval_run_id: retrieval.run_id.clone(),
            history_turns,
            history_note,
            summary_used,
            context_note,
        };
        let result_json =
            serde_json::to_value(&result).map_err(|e| AppError::internal(e.to_string()))?;

        let doc_id = task.document_id.clone().unwrap_or_default();
        // M07 消息粒度属主优先;测试/未走会话流时回退会话粒度。
        let conversation_owner = format!(
            "message:{}",
            task_assistant_message_id.unwrap_or(conversation_id)
        );
        let run_id = retrieval.run_id.clone();
        let citation_req = SaveCitationsRequest {
            owner_type: CitationOwnerType::Message,
            owner_id: conversation_owner,
            document_id: doc_id,
            document_version_id: retrieval.document_version_id.clone(),
            citations: citation_inputs,
            idempotency_key: format!("m09:{}:{}", task.id, run_id),
        };

        let task_id = task.id.clone();
        let attempt_id = attempt.id.clone();
        let validators: OwnerValidators = (*self.citation_owner_checks).clone();
        let commit_outcome = self.tasks.commit_result(
            &task_id,
            &attempt_id,
            result_json,
            outcome,
            None,
            move |conn: &Connection, _task: &TaskRow| -> Result<(), AppError> {
                // Manifest 候选已在检索阶段落库;此处同事务写入 Citation
                // (M05 执行结构校验)。属主校验器由 M07 消息注册。
                save_citations_tx(conn, &citation_req, &validators, &citation_ids_seed, 0)?;
                Ok(())
            },
        )?;
        match commit_outcome {
            CommitOutcome::Committed(t) => Ok(*t),
            CommitOutcome::AlreadyCommitted(t) => Ok(*t),
        }
    }
}

// —— 辅助与文件级结构 ——

/// 本轮摘要层结果。
#[derive(Debug, Default)]
struct SummaryRefresh {
    text: Option<String>,
    /// 摘要失败/截断等用户可见说明(不含正文)。
    notes: Vec<String>,
}

fn summary_failure_note(error_code: &str, previous_used: bool) -> String {
    if previous_used {
        format!("会话摘要更新失败({error_code}),已沿用上次摘要")
    } else {
        format!("会话摘要更新失败({error_code}),本次仅使用最近对话")
    }
}

/// 问候/自我介绍/能力边界:不要求引用,也不附带无关原文来源。
/// 只匹配短寒暄与能力问题,避免把“你好,总结这页”误判成闲聊。
pub(crate) fn is_conversational_question(question: &str) -> bool {
    let normalized: String = question
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !matches!(
                    ch,
                    '。' | '！' | '？' | '!' | '?' | '，' | ',' | '.' | '~' | '～' | '、'
                )
        })
        .flat_map(|ch| ch.to_lowercase())
        .collect();
    if normalized.is_empty() {
        return false;
    }
    const EXACT: &[&str] = &[
        "你好",
        "您好",
        "嗨",
        "哈喽",
        "hello",
        "hi",
        "hey",
        "早上好",
        "下午好",
        "晚上好",
        "晚安",
        "谢谢",
        "多谢",
        "thanks",
        "thankyou",
        "再见",
        "拜拜",
        "bye",
        "在吗",
        "收到",
        "好的",
        "嗯",
        "ok",
        "okay",
    ];
    if EXACT.contains(&normalized.as_str()) {
        return true;
    }
    const CAPABILITY: &[&str] = &[
        "你是谁",
        "你叫什么",
        "你能做什么",
        "你可以做什么",
        "你会做什么",
        "你可以帮我做什么",
        "你能帮我做什么",
        "你能干什么",
        "你有哪些功能",
        "你有什么功能",
        "你的功能",
        "你能帮我吗",
        "介绍你自己",
        "介绍一下你自己",
        "自我介绍",
        "你的能力",
        "能力范围",
        "能做什么",
        "怎么用你",
        "如何使用你",
        "怎么使用你",
    ];
    // 必须完整消费寒暄/能力短语;任何剩余请求(如“总结本文”)都不是纯闲聊。
    let mut rest = normalized.as_str();
    while !rest.is_empty() {
        let prefix = EXACT
            .iter()
            .chain(CAPABILITY.iter())
            .copied()
            .filter(|phrase| rest.starts_with(phrase))
            .max_by_key(|phrase| phrase.len());
        match prefix {
            Some(prefix) => rest = &rest[prefix.len()..],
            None => return false,
        }
    }
    true
}

/// 只把文档请求及其指代追问交给覆盖约束,常识/寒暄不依赖解析状态。
fn requires_document_context(question: &str, history: &[ChatTurn]) -> bool {
    if is_conversational_question(question) {
        return false;
    }
    let question = question.to_lowercase();
    // 注意:"当前页/相邻页/上一页/下一页"这类页级说法本身就是文档范围信号。
    // 曾因为只收录了"这页/本页"而漏掉它们,导致工具里的「对比当前页与相邻页」
    // 被判成闲聊、检索被整段跳过,模型只好回答"没有来源块"。
    // 新增标记必须语义单一:只表达"指向本文内容",不能是泛指词。
    const DOCUMENT_MARKERS: &[&str] = &[
        "本文",
        "文档",
        "原文",
        "文章",
        "资料",
        "论文",
        "作者",
        "章节",
        "这页",
        "本页",
        "此页",
        "该页",
        "当前页",
        "相邻页",
        "上一页",
        "下一页",
        "前后各一页",
        "页内",
        "当前范围",
        "范围内",
        "区间",
        "范围外",
        "这一页",
        "全文",
        "选区",
        "选中",
        "这段",
        "这一段",
        "书中",
        "本书",
        "上文",
        "来源",
        "总结",
        "概括",
        "摘要",
        "讲了什么",
        "讲的什么",
        "第",
        "页码",
        "document",
        "article",
        "paper",
        "chapter",
        "passage",
        "selected",
        "source",
        "summarize",
        "summarise",
        "summary",
        "this page",
        "current page",
        "adjacent page",
        "the text",
        "author",
    ];
    if DOCUMENT_MARKERS
        .iter()
        .any(|marker| question.contains(marker))
    {
        return true;
    }
    let follow_up = [
        "继续",
        "详细",
        "为什么",
        "它",
        "这个",
        "上述",
        "刚才",
        "具体",
        "展开",
        "continue",
        "explain more",
        "why",
        "that",
        "it",
    ]
    .iter()
    .any(|marker| question.contains(marker));
    follow_up
        && last_user_question(history)
            .is_some_and(|previous| requires_document_context(previous, &[]))
}

fn last_user_question(history: &[ChatTurn]) -> Option<&str> {
    history
        .iter()
        .rev()
        .find(|turn| turn.role == ChatTurnRole::User)
        .map(|turn| turn.text.as_str())
}

/// 合并用户可见说明:去空、去重,顺序稳定;没有内容时返回 None。
/// 判断一条说明是否只是内部预算/块数细节(不展示给用户)。
/// 用户明确要求不要出现 "按模型上下文窗口 N tokens 计算预算"、
/// "N 个来源超出单轮块数上限" 这类实现细节。
fn is_budget_note(note: &str) -> bool {
    note.contains("tokens 计算预算")
        || note.contains("超出单轮块数上限")
        || note.contains("单块过长未进入上下文")
        || note.contains("保守预算")
}

fn join_notes(notes: Vec<String>) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for note in notes {
        let note = note.trim();
        if !note.is_empty() && !out.iter().any(|existing| existing == note) {
            out.push(note.to_string());
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join("；"))
    }
}

/// 检索结果:进入 Prompt 的块(原文顺序)+ 全部候选裁决(A9 可解释)。
struct RetrievalOutcome {
    run_id: String,
    included_blocks: Vec<ContentBlockView>,
    #[allow(dead_code)] // 裁决明细已在 persist_candidates 落库;保留供评估导出
    decisions: Vec<CandidateDecision>,
    #[allow(dead_code)]
    capability_mode: CapabilityMode,
    #[allow(dead_code)]
    degraded_path: bool,
    #[allow(dead_code)]
    snapshot_id: String,
    document_version_id: String,
}

fn fail_early(e: AppError) -> (AppError, Option<AttemptRow>) {
    (e, None)
}

fn err_answer(reason: &str) -> AppError {
    m09err::model_output_invalid(reason)
}

struct AnswerCitationInput;

impl AnswerCitationInput {
    fn from_answer(c: &AnswerCitation, answer: &AnswerContent) -> CitationInput {
        // claim kind 取该引文支持的首个段;未匹配默认 inference。
        let kind = c
            .claim_segment_ids
            .iter()
            .find_map(|sid| answer.segments.iter().find(|s| &s.id == sid))
            .map(|s| match s.kind {
                SegmentKind::DocumentFact => ClaimKind::DocumentFact,
                SegmentKind::Inference => ClaimKind::Inference,
                SegmentKind::Example => ClaimKind::Example,
                SegmentKind::UserView => ClaimKind::UserView,
                SegmentKind::GeneralNote => ClaimKind::GeneralNote,
            })
            .unwrap_or(ClaimKind::Inference);
        CitationInput {
            claim_key: c.id.clone(),
            claim_kind: kind,
            source_type: CitationSourceType::ContentBlock,
            source_id: Some(c.block_id.clone()),
            anchor_id: None,
            content_block_id: Some(c.block_id.clone()),
            quoted_text: c.quoted_text.clone(),
            locator: None,
            // 模型自检:支持来源最多 model。
            support_status: None,
            support_source: None,
        }
    }
}

/// AnswerContent 的 JSON Schema(《02》§2:Schema 由模型侧消费)。
pub fn answer_json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["schemaVersion", "status", "segments", "citations"],
        "additionalProperties": false,
        "properties": {
            "schemaVersion": {"const": "answer.v1"},
            "status": {"enum": ["answered", "insufficient_evidence", "conflict", "out_of_scope"]},
            "segments": {"type": "array", "items": {"$ref": "#/definitions/segment"}},
            "citations": {"type": "array", "items": {"$ref": "#/definitions/citation"}},
            "conflicts": {"type": "array", "items": {"$ref": "#/definitions/conflict"}},
            "suggestions": {"type": "array", "items": {"type": "string"}},
            "unverifiedNotes": {"type": "array", "items": {"type": "string"}}
        },
        "definitions": {
            "segment": {
                "type": "object",
                "required": ["id", "kind", "text", "verificationStatus", "citationIds"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "kind": {"enum": ["document_fact", "inference", "example", "user_view", "general_note"]},
                    "text": {"type": "string"},
                    "verificationStatus": {"enum": ["structurally_valid", "unverified"]},
                    "citationIds": {"type": "array", "items": {"type": "string"}}
                }
            },
            "citation": {
                "type": "object",
                "required": ["id", "blockId", "quotedText", "claimSegmentIds"],
                "additionalProperties": false,
                "properties": {
                    "id": {"type": "string"},
                    "blockId": {"type": "string"},
                    "quotedText": {"type": "string"},
                    "claimSegmentIds": {"type": "array", "items": {"type": "string"}}
                }
            },
            "conflict": {
                "type": "object",
                "required": ["description", "citationIds"],
                "additionalProperties": false,
                "properties": {
                    "description": {"type": "string"},
                    "citationIds": {"type": "array", "items": {"type": "string"}}
                }
            }
        }
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::retrieval_ai::models::{StructuredGenerationRequest, StructuredGenerationResult};
    use crate::shared::{NoopEventSink, SystemIdGenerator, TestClock};
    use std::sync::Mutex;

    // 独立临时库,不读取/清理用户数据库。两个 service 的回归测试共用装配。
    pub(crate) fn test_db() -> Arc<Db> {
        let path = std::env::temp_dir().join(format!(
            "kw-service-{}.sqlite3",
            SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations().into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations())
                .chain(crate::evidence::registration::migrations())
                .chain(crate::context::registration::migrations())
                .chain(crate::retrieval_ai::registration::migrations())
                .chain(crate::agent::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql).map_err(|e| crate::persistence::db_app_error(&e))?;
            }
            conn.execute_batch("INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                VALUES ('d1','test','pdf','v1',0,0);
                INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                VALUES ('v1','d1','ready',0);")
                .map_err(|e| crate::persistence::db_app_error(&e))?;
            Ok(())
        }).unwrap();
        db
    }

    struct Snapshot(SnapshotContext);
    impl ContextSnapshotPort for Snapshot {
        fn load(&self, _: &str) -> Result<SnapshotContext, AppError> {
            Ok(self.0.clone())
        }
    }

    struct RecordingGeneration {
        cap: ModelCapabilityView,
        calls: Mutex<Vec<StructuredGenerationRequest>>,
        fail: bool,
        content: Option<serde_json::Value>,
    }
    impl StructuredGenerationPort for RecordingGeneration {
        fn capability(&self, _: &str) -> Result<ModelCapabilityView, AppError> {
            Ok(self.cap.clone())
        }
        fn generate(
            &self,
            request: &StructuredGenerationRequest,
            _: &tokio_util::sync::CancellationToken,
        ) -> Result<StructuredGenerationResult, AppError> {
            self.calls.lock().unwrap().push(request.clone());
            if self.fail {
                return Err(m09err::model_output_invalid("test_unrecoverable"));
            }
            Ok(StructuredGenerationResult {
                content: self.content.clone().unwrap_or_else(|| serde_json::json!({"schemaVersion":"answer.v1", "status":"answered",
                    "segments":[{"id":"s1", "kind":"general_note", "text":format!("模型实际回答: {}",request.question),
                        "verificationStatus":"unverified", "citationIds":[]}], "citations":[]})),
                model_provider_id: "test-provider".into(), model_id: "test-model".into(), partial: false,
            })
        }
    }
    fn generator() -> RecordingGeneration {
        RecordingGeneration {
            cap: ModelCapabilityView {
                generation_available: true,
                generation_authorized: true,
                provider_id: Some("test-provider".into()),
                model_id: Some("test-model".into()),
            },
            calls: Mutex::new(Vec::new()),
            fail: false,
            content: None,
        }
    }
    fn snapshot() -> SnapshotContext {
        SnapshotContext {
            snapshot_id: "snap".into(),
            document_id: "d1".into(),
            document_version_id: "v1".into(),
            parse_generation_id: None,
            scope_kind: SnapshotScopeKind::FullText,
            explicit_block_ids: vec![],
            note_sources: vec![],
            coverage_complete: false,
        }
    }
    fn service(
        generation: Arc<RecordingGeneration>,
        snapshot: SnapshotContext,
    ) -> RetrievalAiService {
        let db = test_db();
        let clock = Arc::new(TestClock::new(1000));
        let ids = Arc::new(SystemIdGenerator);
        let events = Arc::new(NoopEventSink);
        let tasks = Arc::new(TaskService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let parse = Arc::new(ParseIndexService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
            tasks.clone(),
        ));
        let evidence = Arc::new(EvidenceService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            parse.clone(),
        ));
        let mut owners = OwnerValidators::default();
        owners.register(CitationOwnerType::Message, |_, _, _| Ok(true));
        RetrievalAiService::new(
            db,
            clock,
            ids,
            events,
            tasks,
            parse,
            evidence,
            Arc::new(Snapshot(snapshot)),
            generation,
        )
        .with_citation_owner_validators(Arc::new(owners))
    }
    fn request(question: &str) -> AskRequest {
        AskRequest {
            document_id: "d1".into(),
            conversation_id: "conversation".into(),
            assistant_message_id: None,
            snapshot_id: "snap".into(),
            question: question.into(),
            locale: "zh-CN".into(),
            idempotency_key: SystemIdGenerator.new_entity_id(),
            history: vec![],
            attachments: vec![],
        }
    }

    #[test]
    fn greetings_and_capabilities_must_consume_the_whole_question() {
        for text in [
            "你好！",
            "你好，你能做什么？",
            "介绍一下你自己",
            "Hello!",
            "你是谁？",
            "你能帮我做什么",
            "你可以帮我做什么？",
            "你有什么功能",
            "自我介绍",
        ] {
            assert!(is_conversational_question(text), "{text}");
        }
        for text in [
            "你好，总结这页",
            "你好你能做什么，总结本文",
            "你能做什么，结合本文总结",
            "你是谁？请解释文章结论",
            "hi, summarize this document",
        ] {
            assert!(!is_conversational_question(text), "{text}");
            assert!(requires_document_context(text, &[]), "{text}");
        }
    }

    #[test]
    fn page_level_tool_prompts_count_as_document_questions() {
        // 工具栏「对比相邻页」等提示词不含"本文/文档",但依然是文档问题:
        // 曾因此跳过检索,模型只能回答"没有来源块"。
        for text in [
            "请对比当前页与相邻页的核心差异,并给出引用。",
            "请解释当前范围内的核心内容,并给出引用。",
            "对比当前页和上一页",
        ] {
            assert!(requires_document_context(text, &[]), "{text}");
        }
        // 泛指常识不能被误判成文档问题(仍由模型直接回答)。
        assert!(!requires_document_context("什么是光合作用?", &[]));
    }

    #[test]
    fn document_followups_keep_coverage_constraints_but_general_questions_do_not() {
        let history = vec![ChatTurn {
            role: ChatTurnRole::User,
            text: "总结本文".into(),
        }];
        assert!(requires_document_context("继续展开", &history));
        assert!(!requires_document_context("你好", &history));
        assert!(!requires_document_context("什么是光合作用？", &history));
    }

    #[test]
    fn no_source_greeting_introduction_capability_and_general_questions_really_generate() {
        let generation = Arc::new(generator());
        let service = service(generation.clone(), snapshot());
        for question in ["你好", "介绍一下你自己", "你能做什么", "什么是光合作用？"]
        {
            let task = service.ask(request(question)).unwrap();
            assert_eq!(task.status, TaskStatus::Succeeded);
            let answer = service.task_answer(&task.id).unwrap().unwrap();
            let normalized = parse_query_with_previous(question, None).normalized_question;
            assert_eq!(
                answer.segments[0].text,
                format!("模型实际回答: {normalized}")
            );
        }
        let calls = generation.calls.lock().unwrap();
        assert_eq!(calls.len(), 4);
        assert!(calls
            .iter()
            .all(|call| call.inputs.is_empty() && call.document_map.is_none()));
        assert!(calls.iter().all(|call| call.document_id == "d1"
            && call.snapshot_id == "snap"
            && !call.attempt_id.is_empty()));
    }

    #[test]
    fn no_source_questions_cannot_bypass_privacy_or_model_availability() {
        for (available, authorized, code) in [
            (true, false, "PRIVACY_GRANT_REQUIRED"),
            (false, true, "MODEL_UNAVAILABLE"),
        ] {
            let mut generation = generator();
            generation.cap.generation_available = available;
            generation.cap.generation_authorized = authorized;
            let generation = Arc::new(generation);
            let service = service(generation.clone(), snapshot());
            for question in ["你好", "什么是光合作用？"] {
                assert_eq!(service.ask(request(question)).unwrap_err().code, code);
            }
            assert!(generation.calls.lock().unwrap().is_empty());
        }
    }

    #[test]
    fn mixed_document_question_requires_coverage_before_generation() {
        let generation = Arc::new(generator());
        let service = service(generation.clone(), snapshot());
        assert_eq!(
            service
                .ask(request("你好你能做什么，总结本文"))
                .unwrap_err()
                .code,
            "CONTEXT_COVERAGE_INCOMPLETE"
        );
        assert!(generation.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn mixed_question_preserves_selected_sources() {
        let generation = Arc::new(generator());
        let mut snap = snapshot();
        snap.scope_kind = SnapshotScopeKind::ExplicitBlocks;
        let mut service = service(generation.clone(), snap.clone());
        let block = service
            .parse
            .ensure_selection_blocks(&crate::parse_index::models::SelectionBlockRequest {
                document_id: "d1".into(),
                version_id: "v1".into(),
                text: "本文讨论植物如何利用阳光。".into(),
                locator: crate::shared::SourceLocator::pdf(
                    1,
                    vec![],
                    crate::shared::locator::PageRotation::R0,
                ),
                heading_path: vec![],
            })
            .unwrap();
        snap.explicit_block_ids.push(block.id.clone());
        service.snapshots = Arc::new(Snapshot(snap));
        service.ask(request("你能做什么，结合本文总结")).unwrap();
        let calls = generation.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0]
            .inputs
            .iter()
            .any(|input| input.source_id == block.id));
    }

    /// 混合问题的寒暄词匹配不到任何原文块时,仍必须在该快照冻结的范围内
    /// 回填正文,而不是把用户选中的范围当成“没有正文”。
    #[test]
    fn conversational_words_do_not_blank_a_local_scope() {
        let generation = Arc::new(generator());
        let mut snap = snapshot();
        snap.scope_kind = SnapshotScopeKind::Local;
        snap.coverage_complete = true;
        let service = service(generation.clone(), snapshot());
        // 可检索代次:一个 indexable=1 的普通正文块 + 对应 FTS 行,
        // 与真实解析产出同形(按需选区块不可索引)。
        let block_id = "blk-local".to_string();
        let text = "快照不可变，追问可以复用同一快照。".to_string();
        let gen_id = "gen-local".to_string();
        service
            .db
            .write({
                let block_id = block_id.clone();
                let text = text.clone();
                let gen_id = gen_id.clone();
                move |conn| {
                    conn.execute(
                        "INSERT INTO parse_generation(id, document_version_id, parser_config_hash,                          parser_version, status, is_current, started_at)                          VALUES (?1,'v1','fixture','fixture','complete',1,0)",
                        [&gen_id],
                    )
                    .map_err(|e| crate::persistence::db_app_error(&e))?;
                    conn.execute(
                        "INSERT INTO content_block(id, document_version_id, parse_generation_id,                          origin, block_key, ordinal, kind, text, normalized_text, text_hash,                          normalized_text_hash, heading_path_json, locator_json, completeness,                          indexable, normalization_version, created_at)                          VALUES (?1,'v1',?2,'parser','k1',0,'paragraph',?3,?3,'h','h','[]',?4,                         'complete',1,'v1',0)",
                        rusqlite::params![
                            block_id,
                            gen_id,
                            text,
                            crate::shared::SourceLocator::pdf(
                                1,
                                vec![],
                                crate::shared::locator::PageRotation::R0
                            )
                            .canonical_string()
                        ],
                    )
                    .map_err(|e| crate::persistence::db_app_error(&e))?;
                    conn.execute(
                        "INSERT INTO content_block_fts(text, block_id, parse_generation_id)                          VALUES (?1,?2,?3)",
                        rusqlite::params![text, block_id, gen_id],
                    )
                    .map_err(|e| crate::persistence::db_app_error(&e))?;
                    Ok(())
                }
            })
            .unwrap();
        snap.parse_generation_id = Some(gen_id);
        snap.explicit_block_ids.push(block_id.clone());
        let mut service = service;
        service.snapshots = Arc::new(Snapshot(snap));
        // 关键词全部来自寒暄与请求动词:任何块都不会命中。
        service.ask(request("你好，结合本文总结一下")).unwrap();
        let calls = generation.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0]
                .inputs
                .iter()
                .any(|input| input.source_id == block_id),
            "快照范围内的正文必须进入 Prompt"
        );
    }

    #[test]
    fn no_source_generation_error_is_not_replaced_by_an_answer() {
        let mut generation = generator();
        generation.fail = true;
        let generation = Arc::new(generation);
        let service = service(generation.clone(), snapshot());
        assert_eq!(
            service.ask(request("你好")).unwrap_err().code,
            "MODEL_OUTPUT_INVALID"
        );
        assert_eq!(generation.calls.lock().unwrap().len(), 1);
        let results: i64 = service
            .db
            .read(|conn| {
                conn.query_row("SELECT COUNT(*) FROM task_result", [], |r| r.get(0))
                    .map_err(|e| crate::persistence::db_app_error(&e))
            })
            .unwrap();
        assert_eq!(results, 0);
    }

    #[test]
    fn empty_selected_sources_do_not_short_circuit_document_generation() {
        let generation = Arc::new(generator());
        let mut snap = snapshot();
        snap.scope_kind = SnapshotScopeKind::ExplicitBlocks;
        let service = service(generation.clone(), snap);
        let task = service.ask(request("总结选区")).unwrap();
        assert_eq!(task.status, TaskStatus::Succeeded);
        let calls = generation.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].inputs.is_empty());
    }

    #[test]
    fn unrecoverable_returned_content_fails_without_storing_an_answer() {
        for content in [
            serde_json::json!({"schemaVersion":"answer.v1","status":"answered","segments":[],"citations":[]}),
            serde_json::json!({"schemaVersion":"plain_text_fallback.v1","text":"{\"schemaVersion\":\"answer.v1\",\"segments\":[]}"}),
        ] {
            let mut generation = generator();
            generation.content = Some(content);
            let generation = Arc::new(generation);
            let service = service(generation.clone(), snapshot());
            assert_eq!(
                service.ask(request("你好")).unwrap_err().code,
                "MODEL_OUTPUT_INVALID"
            );
            assert_eq!(generation.calls.lock().unwrap().len(), 1);
            let results: i64 = service
                .db
                .read(|conn| {
                    conn.query_row("SELECT COUNT(*) FROM task_result", [], |r| r.get(0))
                        .map_err(|e| crate::persistence::db_app_error(&e))
                })
                .unwrap();
            assert_eq!(results, 0);
        }
    }
}
