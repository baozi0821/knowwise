//! M13 应用壳(唯一组装根)。命令注册总表、事件接线与模块初始化顺序
//! 都归 M13 所有,其他模块不得创建 Builder(《M13》§3)。
//!
//! 组装根保持薄:本模块默认特性下也编译(不含 Tauri 依赖的部分:
//! 命令名契约、安全配置契约、壳层状态机、数据库装配),使 `cargo test`
//! 即可运行 M13 契约测试;Tauri Builder、事件广播与路径解析位于
//! `desktop` feature 之后。任何业务规则(解析、检索、证据校验、笔记
//! 保存、产物版本)都不得在此实现。

pub mod command_names;
pub mod ports;
pub mod quiz_port;
// capture:原生整屏截图命令(纯 FFI 实现默认特性下也可编译/测试,
// `#[tauri::command]` 包装在 desktop feature 之后)。
pub mod screenshot;
pub mod security_config;
pub mod shell;

#[cfg(feature = "desktop")]
pub mod ask_flow;
#[cfg(feature = "desktop")]
pub mod command_registry;
// 后台编排契约也在默认特性测试中运行,不依赖 WebView。
#[cfg(any(feature = "desktop", test))]
pub mod generation_flow;
#[cfg(feature = "desktop")]
pub mod streaming;

use std::sync::Arc;

use crate::agent::reasoning::{AgentReasoningClearContributor, ReasoningService};
use crate::agent::service::AgentService;
use crate::artifacts::models::{ArtifactCodec, ArtifactVersionOrigin};
use crate::artifacts::{ArtifactService, SummaryV1Codec};
use crate::attachments::{AttachmentService, ATTACHMENT_ORPHAN_TTL_MS};
use crate::context::service::ContextService;
use crate::document::detect::SizeLimits;
use crate::document::{DocumentService, ReaderService};
use crate::evidence::EvidenceService;
use crate::graph::service::GraphService;
use crate::insights::service::CardsClearContributor;
use crate::insights::CardService;
use crate::metrics::RunMetricsService;
use crate::model_privacy::clear::{EvidenceRetentionAdvisor, NotesClearContributor};
use crate::model_privacy::{DefaultProviderFactory, ModelPrivacyService, SystemSecretStore};
use crate::notes::NotesService;
use crate::parse_index::ParseIndexService;
use crate::persistence::{Db, MigrationRunner, TaskService};
use crate::quiz::QuizService;
use crate::retrieval_ai::ports::{ContextSnapshotPort, StructuredGenerationPort};
use crate::retrieval_ai::RetrievalAiService;
use crate::shared::settings::{SettingsStore, SettingsV1};
use crate::shared::{AppError, Clock, CoreErrorCode, EventSink, SystemClock, SystemIdGenerator};
use crate::translation::service::TranslationMemoryClearContributor;
use crate::translation::TranslationService;
use crate::CoreState;

/// 组装后的应用状态:命令层只读这里,不自行打开数据库。
#[derive(Clone)]
pub struct AppState {
    pub core: CoreState,
    pub db: Arc<Db>,
    pub tasks: Arc<TaskService>,
    pub documents: Arc<DocumentService>,
    pub reader: Arc<ReaderService>,
    pub parse: Arc<ParseIndexService>,
    pub evidence: Arc<EvidenceService>,
    pub notes: Arc<NotesService>,
    /// M07 上下文快照与会话(替代组装占位:真实快照/消息)。
    pub context: Arc<ContextService>,
    /// M15 图片附件(落盘/配额/摘要校验/孤儿清理)。
    pub attachments: Arc<AttachmentService>,
    /// M08 模型接入、隐私与设置(唯一网络出口 EgressGateway)。
    pub model_privacy: Arc<ModelPrivacyService>,
    pub artifacts: Arc<ArtifactService>,
    pub graphs: Arc<GraphService>,
    pub quiz: Arc<QuizService>,
    pub retrieval: Arc<RetrievalAiService>,
    /// P2 Agent 只读循环(深度研究;默认问答仍走 retrieval)。
    pub agent: Arc<AgentService>,
    /// 结构化生成端口(与 M09 共用同一 EgressGateway 适配器;
    /// 产物生成命令直接复用,不新建 HTTP 客户端)。
    pub generation: Arc<dyn StructuredGenerationPort>,
    /// 2A 文档画像与动态卡片(缓存;每条事实带来源)。
    pub cards: Arc<CardService>,
    /// 2C 翻译(translation.v1;翻译记忆/术语/部分失败)。
    pub translation: Arc<TranslationService>,
    /// 离线翻译引擎(OPUS-MT/ONNX):本地优先,不联网、不调大模型。
    pub local_translation: Arc<crate::translation::offline::LocalTranslator>,
    /// P0 1C:每次流式运行的耗时/比例指标(不含正文)。
    pub metrics: Arc<RunMetricsService>,
    /// P0 1E:可选保存的思考记录(默认不写;纳入隐私清除)。
    pub reasoning: Arc<ReasoningService>,
    /// 启动时生效的非敏感设置(导入上限、本地解析自动重试)。
    pub settings: SettingsV1,
}

/// 按依赖顺序汇总模块迁移(《01》§12)。模块只提供清单,
/// 注册顺序由 M13 拥有;重复 (module, version) 由 MigrationRunner 拒绝。
pub fn all_migrations() -> Vec<crate::persistence::Migration> {
    let mut sets = crate::persistence::registration::migrations();
    sets.extend(crate::document::registration::migrations());
    sets.extend(crate::document::reader_registration::migrations());
    sets.extend(crate::parse_index::registration::migrations());
    sets.extend(crate::evidence::registration::migrations());
    sets.extend(crate::notes::registration::migrations());
    sets.extend(crate::context::registration::migrations()); // M07:快照/会话/消息
    sets.extend(crate::attachments::registration::migrations()); // M15:图片附件
    sets.extend(crate::agent::registration::migrations()); // M16:Agent 运行/步骤
    sets.extend(crate::model_privacy::registration::migrations()); // M08:授权/清除审计
    sets.extend(crate::quiz::registration::migrations()); // M11:作答记录与无效题反馈
    sets.extend(crate::retrieval_ai::registration::migrations()); // M09:检索运行与候选
    sets.extend(crate::artifacts::registration::migrations()); // M10:产物外壳/版本/导出记录
    sets.extend(crate::graph::registration::migrations()); // M12:无业务表,占位汇总
    sets.extend(crate::insights::registration::migrations()); // 2A:画像/卡片
    sets.extend(crate::translation::registration::migrations()); // 2C:翻译记忆
    sets.extend(crate::metrics::registration::migrations()); // M13:运行指标
    sets
}

/// 完整启动装配(桌面 setup 与集成测试共用,《M13》§2 顺序):
/// 设置文件 → 数据库连接 → quick_check → 迁移(先备份)→ 任务恢复 → 服务装配。
///
/// - `events`:桌面传入 TauriEventSink(COMMIT 后广播到 WebView);测试传替身。
/// - 设置不可解析 → Err(SETTINGS_INVALID),调用方进入恢复页,不写业务状态。
/// - 数据库损坏/迁移失败 → Err(DB_CORRUPT/DB_MIGRATION_FAILED);迁移前
///   备份已由 MigrationRunner 保留原文件与可恢复副本(M01 §8)。
///
/// 构造离线翻译引擎持有者。
///
/// 模型目录:`<应用数据目录>/models/translate/<源>-<目标>/`。
/// 应用只读不下载;模型缺失时状态显式为 `ModelMissing`,由 UI 提示安装,
/// 不会静默改用远程模型。
fn build_local_translator(
    data_dir: &std::path::Path,
) -> crate::translation::offline::LocalTranslator {
    let root = data_dir.join("models").join("translate");
    crate::translation::offline::LocalTranslator::new(root, |dir| {
        #[cfg(feature = "local-mt")]
        {
            crate::translation::onnx_mt::create_engine(dir)
        }
        #[cfg(not(feature = "local-mt"))]
        {
            let _ = dir;
            None
        }
    })
}

pub fn bootstrap_with(
    data_dir: &std::path::Path,
    config_dir: &std::path::Path,
    events: Arc<dyn EventSink>,
) -> Result<AppState, AppError> {
    let settings = shell::load_settings_or_fail(&SettingsStore::open(config_dir))?;

    std::fs::create_dir_all(data_dir).map_err(|e| AppError::internal(e.to_string()))?;
    let db_path = data_dir.join("knowwise.sqlite3");
    // 打开连接时也可能直接读到损坏/非数据库文件(PRAGMA 阶段触发
    // SQLITE_NOTADB);必须走统一的 db_app_error 映射,否则恢复页
    // 只能看到 INTERNAL,无法给出 DB_CORRUPT 指引。
    let db = Db::open(&db_path, 4).map_err(|e| match e {
        crate::persistence::connection::DbError::Sqlite(ref sqlite) => {
            crate::persistence::db_app_error(sqlite)
        }
        other => AppError::internal(other.to_string()),
    })?;

    let clock = Arc::new(SystemClock);
    let now = clock.now_ms();

    // 启动快速完整性检查;失败进入恢复模式,不继续写入(M01 §8)。
    db.write(|conn| {
        let ok: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(|e| crate::persistence::db_app_error(&e))?;
        if ok != "ok" {
            return Err(AppError::new(CoreErrorCode::DbCorrupt, "bootstrap"));
        }
        Ok(())
    })?;

    let runner = MigrationRunner::new(data_dir.join("backups"));
    let sets = all_migrations();
    db.write(move |conn| {
        runner.run(conn, &sets, &db_path, now).map_err(|e| {
            AppError::new(CoreErrorCode::DbMigrationFailed, "bootstrap").with_detail(
                "reason",
                crate::shared::error::DetailValue::Str(e.to_string()),
            )
        })
    })?;

    let db = Arc::new(db);
    let core = CoreState {
        clock,
        ids: Arc::new(SystemIdGenerator),
        events,
    };
    let tasks = Arc::new(TaskService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
    ));

    // 启动恢复:活动任务 → interrupted;是否自动补发本地解析由设置决定,
    // 远程生成永不自动恢复(M01 §7 / settings.auto_retry_parse_on_startup)。
    tasks.recover_interrupted_tasks(settings.auto_retry_parse_on_startup)?;
    let documents = Arc::new(DocumentService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
        SizeLimits {
            pdf_mib: settings.max_pdf_mib,
            docx_mib: settings.max_docx_mib,
            markdown_mib: settings.max_markdown_mib,
        },
    ));
    // 导入中途崩溃遗留的 computing 版本标记 unreadable(M02 §3)。
    let _ = documents.recover_imports();
    let reader = Arc::new(ReaderService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
        Arc::clone(&documents),
    ));
    let parse = Arc::new(ParseIndexService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
        Arc::clone(&tasks),
    ));
    // 遗留 parsing/indexing 代次 → failed;重试复用同代次(M04 §3)。
    let _ = parse.recover_generations();
    let evidence = Arc::new(EvidenceService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        Arc::clone(&parse),
    ));
    let notes = Arc::new(NotesService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
    ));
    // M07 上下文快照与会话(读取 M04 块投影,不写索引)。
    let context = Arc::new(ContextService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
        Arc::clone(&parse),
    ));
    // M15 图片附件:字节落应用数据目录,库中只存引用/摘要。
    let attachments = Arc::new(AttachmentService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        data_dir.join("attachments"),
    ));
    // 启动兜底:清理超过 TTL 的未发送附件与磁盘上无记录的孤儿文件。
    let _ = attachments.sweep_orphans(now - ATTACHMENT_ORPHAN_TTL_MS);
    // M08 模型/隐私服务(唯一网络出口、授权裁决、清除协调、诊断);
    // 密钥走系统凭据存储,设置存 config/model_settings.json。
    let paths = crate::shared::paths::AppPaths::from_base(data_dir);
    let _ = paths.ensure_created();
    let model_privacy = Arc::new(ModelPrivacyService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
        Arc::clone(&tasks),
        &paths.config_dir,
        data_dir,
        Arc::new(SystemSecretStore::new()),
        Arc::new(DefaultProviderFactory::default()),
    )?);
    // 清除协调接线:M06 用户资产贡献器 + M05 证据保留裁决
    //(M07 会话/M10 产物/M04 索引缓存的贡献器在其提供接口后注册)。
    model_privacy
        .clear_coordinator()
        .register_contributor(Arc::new(NotesClearContributor::new(Arc::clone(&notes))));
    let reasoning = Arc::new(ReasoningService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
    ));
    model_privacy
        .clear_coordinator()
        .register_contributor(Arc::new(AgentReasoningClearContributor::new(Arc::clone(
            &reasoning,
        ))));
    model_privacy
        .clear_coordinator()
        .register_contributor(Arc::new(CardsClearContributor::new(Arc::clone(&db))));
    model_privacy
        .clear_coordinator()
        .register_contributor(Arc::new(TranslationMemoryClearContributor::new(
            Arc::clone(&db),
        )));
    model_privacy
        .clear_coordinator()
        .set_retention_advisor(Arc::new(EvidenceRetentionAdvisor::new(Arc::clone(
            &evidence,
        ))));
    let metrics = Arc::new(RunMetricsService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
    ));
    let graphs = Arc::new(GraphService::new(Arc::clone(&db)));
    let quiz = Arc::new(QuizService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
    ));
    let artifacts = Arc::new(ArtifactService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        core.events.clone(),
    ));
    // Codec 注册(M10 §4):内置 summary.v1;M11 quiz.v1 与 M12 graph.v1
    // 逐字段转发注册。未注册 kind 的内容不能写入产物。
    artifacts.register_codec(Arc::new(SummaryV1Codec));
    struct QuizCodecForwarding;
    impl ArtifactCodec for QuizCodecForwarding {
        fn kind(&self) -> &str {
            crate::quiz::QuizV1Codec.kind()
        }
        fn schema_version(&self) -> &str {
            crate::quiz::QuizV1Codec.schema_version()
        }
        fn validate(&self, content: &serde_json::Value) -> bool {
            match content.get("schemaVersion").and_then(|v| v.as_str()) {
                Some("quiz.v2") => crate::quiz::v2::QuizV2Codec.validate(content),
                _ => crate::quiz::QuizV1Codec.validate(content),
            }
        }
    }
    artifacts.register_codec(Arc::new(QuizCodecForwarding));
    struct GraphCodecForwarding;
    impl ArtifactCodec for GraphCodecForwarding {
        fn kind(&self) -> &str {
            crate::graph::validate::GraphV1Codec.kind()
        }
        fn schema_version(&self) -> &str {
            crate::graph::validate::GraphV1Codec.schema_version()
        }
        fn validate(&self, content: &serde_json::Value) -> bool {
            crate::graph::validate::GraphV1Codec.validate(content)
        }
    }
    artifacts.register_codec(Arc::new(GraphCodecForwarding));
    struct TranslationCodecForwarding;
    impl ArtifactCodec for TranslationCodecForwarding {
        fn kind(&self) -> &str {
            crate::translation::models::TranslationV1Codec.kind()
        }
        fn schema_version(&self) -> &str {
            crate::translation::models::TranslationV1Codec.schema_version()
        }
        fn validate(&self, content: &serde_json::Value) -> bool {
            crate::translation::models::TranslationV1Codec.validate(content)
        }
    }
    artifacts.register_codec(Arc::new(TranslationCodecForwarding));

    // M13 接线:Citation 属主校验器(M06 note/clarification、M07 message、
    // M10 artifact_version 的存在性;同一事务内可见未提交行)。
    evidence.register_owner_validator(
        crate::evidence::CitationOwnerType::Message,
        crate::context::service::message_owner_exists_tx,
    );
    evidence.register_owner_validator(
        crate::evidence::CitationOwnerType::Note,
        crate::notes::service::note_owner_exists_tx,
    );
    evidence.register_owner_validator(
        crate::evidence::CitationOwnerType::Clarification,
        crate::notes::service::clarification_owner_exists_tx,
    );
    evidence.register_owner_validator(
        crate::evidence::CitationOwnerType::ArtifactVersion,
        crate::artifacts::service::ArtifactService::artifact_version_owner_exists_tx,
    );

    // M11 接线:QuizArtifactSource 由 M10 ArtifactService 投影提供。
    struct ArtifactBackedQuizSource {
        service: Arc<ArtifactService>,
    }
    impl crate::quiz::models::QuizArtifactSource for ArtifactBackedQuizSource {
        fn load_quiz_version(
            &self,
            artifact_version_id: &str,
        ) -> Result<Option<crate::quiz::models::QuizArtifactVersion>, AppError> {
            let Some(brief) = self.service.find_version(artifact_version_id)? else {
                return Ok(None);
            };
            if brief.kind != "quiz" {
                return Ok(None);
            }
            let origin = match brief.origin {
                ArtifactVersionOrigin::AiDraft => crate::quiz::models::QuizArtifactOrigin::AiDraft,
                ArtifactVersionOrigin::UserEdit => {
                    crate::quiz::models::QuizArtifactOrigin::UserEdit
                }
                ArtifactVersionOrigin::Copy => crate::quiz::models::QuizArtifactOrigin::Copy,
                ArtifactVersionOrigin::Restore => crate::quiz::models::QuizArtifactOrigin::Restore,
            };
            Ok(Some(crate::quiz::models::QuizArtifactVersion {
                id: brief.version_id,
                artifact_id: brief.artifact_id,
                document_id: brief.document_id,
                schema_version: brief.schema_version,
                origin,
                content_json: brief.content_json,
            }))
        }
    }
    quiz.register_artifact_source(Arc::new(ArtifactBackedQuizSource {
        service: Arc::clone(&artifacts),
    }));

    // M12 接线:GraphVersionSource 由 M10 ArtifactService 投影提供
    // (graph_export 读取待导出的 artifact_version 内容)。
    struct ArtifactBackedGraphSource {
        service: Arc<ArtifactService>,
    }
    impl crate::graph::service::GraphVersionSource for ArtifactBackedGraphSource {
        fn load_graph_version(
            &self,
            artifact_id: &str,
            version_id: &str,
        ) -> Result<crate::graph::service::GraphVersionContent, AppError> {
            let Some(brief) = self.service.find_version(version_id)? else {
                return Err(AppError::new(CoreErrorCode::NotFound, "graph-source"));
            };
            if brief.artifact_id != artifact_id {
                return Err(AppError::new(CoreErrorCode::NotFound, "graph-source"));
            }
            if brief.kind != "graph" {
                return Err(AppError::new(CoreErrorCode::InvalidRequest, "graph-source")
                    .with_detail("reason", "artifact_kind_not_graph".into()));
            }
            let content: crate::graph::models::GraphContent =
                serde_json::from_str(&brief.content_json).map_err(|_| {
                    crate::graph::models::graph_error(
                        crate::graph::models::GraphErrorCode::GraphSchemaInvalid,
                        "graph-source",
                    )
                })?;
            Ok(crate::graph::service::GraphVersionContent {
                document_id: brief.document_id,
                content,
            })
        }
    }
    graphs.register_version_source(Arc::new(ArtifactBackedGraphSource {
        service: Arc::clone(&artifacts),
    }));
    // M09 端口接线(占位 Port 已由 M07/M08 真实实现取代):
    // 快照 → M07 不可变快照;生成/嵌入 → M08 EgressGateway。
    // Citation 属主校验器必须传入同一份注册表:M09 在自己的提交事务里
    // 写 Citation,空注册表会以 owner_validator_missing 拒绝(而非默认放行)。
    let owner_validators = evidence.owner_validators_snapshot();
    let generation: Arc<dyn StructuredGenerationPort> = Arc::new(
        ports::GenerationPortAdapter::new(Arc::clone(&model_privacy)),
    );
    let translation = Arc::new(TranslationService::new(
        Arc::clone(&db),
        core.clock.clone(),
        core.ids.clone(),
        Arc::clone(&parse),
        Arc::clone(&generation),
        Arc::clone(&artifacts),
    ));
    let local_translation = Arc::new(build_local_translator(data_dir));
    let cards = Arc::new(CardService::new(
        Arc::clone(&db),
        core.clock.clone(),
        Arc::clone(&documents),
        Arc::clone(&parse),
        Some(Arc::clone(&generation)),
    ));
    let retrieval = Arc::new(
        RetrievalAiService::new(
            Arc::clone(&db),
            core.clock.clone(),
            core.ids.clone(),
            core.events.clone(),
            Arc::clone(&tasks),
            Arc::clone(&parse),
            Arc::clone(&evidence),
            Arc::new(ports::ContextSnapshotAdapter::new(Arc::clone(&context)))
                as Arc<dyn ContextSnapshotPort>,
            Arc::clone(&generation),
        )
        .with_citation_owner_validators(owner_validators)
        // M07 滚动摘要:窗口与持久化归 M07,模型调用仍走 M09→M08 许可链。
        .with_conversation_summary_port(Arc::new(ports::ConversationSummaryAdapter::new(
            Arc::clone(&context),
        ))),
    );
    // P2 Agent 只读循环:每步经同一 EgressGateway;默认问答不受影响。
    let agent = Arc::new(
        AgentService::with_gateway(
            Arc::clone(&db),
            core.clock.clone(),
            core.ids.clone(),
            Arc::clone(&tasks),
            Arc::clone(&parse),
            Arc::clone(&artifacts),
            Arc::clone(&model_privacy),
        )
        // P3A:写工具确认事件 + create_quiz 复用 M09 生成端口 + Citation 落库。
        .with_write_context(core.events.clone(), Arc::clone(&generation))
        .with_evidence(Arc::clone(&evidence)),
    );
    Ok(AppState {
        local_translation,
        core,
        db,
        tasks,
        documents,
        reader,
        parse,
        evidence,
        notes,
        context,
        attachments,
        model_privacy,
        artifacts,
        cards,
        translation,
        graphs,
        quiz,
        retrieval,
        agent,
        generation,
        metrics,
        reasoning,
        settings,
    })
}

/// 测试便利入口:默认设置 + 空事件出口(桌面 setup 使用 bootstrap_with)。
pub fn bootstrap_database(data_dir: &std::path::Path) -> Result<AppState, AppError> {
    let events: Arc<dyn EventSink> = Arc::new(crate::shared::NoopEventSink);
    let config_dir = data_dir.join("config-for-tests");
    // 测试场景不落盘设置文件,使用默认值;设置校验路径见 shell.rs 测试。
    std::fs::create_dir_all(&config_dir).map_err(|e| AppError::internal(e.to_string()))?;
    bootstrap_with(data_dir, &config_dir, events)
}

#[cfg(feature = "desktop")]
pub mod desktop {
    //! Tauri 侧装配辅助(仅 desktop feature):路径解析、事件广播、日志。
    use tauri::Manager;

    use super::shell::DOMAIN_EVENT_CHANNEL;
    use crate::shared::events::{DomainEvent, EventSink};

    /// 测试/验收专用覆盖:显式设置时完全隔离用户库与配置。
    /// 生产不设置该变量,仍使用 Windows Known Folder(Tauri app_data_dir)。
    fn env_override(key: &str) -> Option<std::path::PathBuf> {
        std::env::var_os(key)
            .map(std::path::PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
    }

    /// 供 setup 使用的路径解析(Tauri 平台路径,业务模块不拼接主目录)。
    pub fn resolve_data_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
        if let Some(dir) = env_override("KNOWWISE_DATA_DIR") {
            return dir;
        }
        app.path()
            .app_data_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
    }

    pub fn resolve_config_dir(app: &tauri::AppHandle) -> std::path::PathBuf {
        if let Some(dir) = env_override("KNOWWISE_CONFIG_DIR") {
            return dir;
        }
        // 只覆盖数据目录时,配置也留在同一隔离目录内,避免读写用户配置。
        if let Some(dir) = env_override("KNOWWISE_DATA_DIR") {
            return dir.join("config-for-tests");
        }
        app.path()
            .app_config_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
    }

    /// 事件出口的 Tauri 实现:服务层在数据库 COMMIT 后调用 emit,
    /// 这里把 DomainEvent 序列化广播到唯一 WebView 通道。
    /// 序列化失败/无窗口只记日志——事件是通知,丢失由查询兜底(《02》§4)。
    pub struct TauriEventSink {
        app: tauri::AppHandle,
    }

    impl TauriEventSink {
        pub fn new(app: tauri::AppHandle) -> Self {
            Self { app }
        }
    }

    impl EventSink for TauriEventSink {
        fn emit(&self, event: DomainEvent) {
            use tauri::Emitter;
            if let Err(e) = self.app.emit(DOMAIN_EVENT_CHANNEL, &event) {
                tracing::warn!(
                    error_code = "EVENT_EMIT_FAILED",
                    "domain event emit failed: {e}"
                );
            }
        }
    }

    /// 脱敏日志初始化(白名单字段过滤器见 shared::logging;幂等)。
    pub fn init_logging() {
        let _ = tracing_subscriber::fmt()
            .event_format(crate::shared::logging::RedactingFormatter)
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .try_init();
    }
}
