//! 2A 文档画像与动态卡片服务。
//!
//! - 画像与卡片都是可重建缓存:键包含版本 + 解析代次 + 页/语言/种类;
//!   解析代次或块集合变化会产生新 profileVersion,旧缓存自然失效。
//! - 每条 document_fact 必须有可定位 sourceRefs(块/页/标题/原文);
//!   AI 推断必须标记 ai_inference。
//! - 模型不可用/未授权/输出无效时降级为抽取式卡片,绝不使用固定正文。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};

use crate::document::models::DocumentFormat;
use crate::document::DocumentService;
use crate::parse_index::models::ContentBlockView;
use crate::parse_index::ParseIndexService;
use crate::persistence::{db_app_error, Db};
use crate::retrieval_ai::models::{
    GenerationInput, SourceRole, StructuredGenerationRequest, TaskKind,
};
use crate::retrieval_ai::ports::StructuredGenerationPort;
use crate::shared::stream::noop_observer;
use crate::shared::textnorm::sha256_str;
use crate::shared::{AppError, Clock, CoreErrorCode};

use super::models::{
    CardSourceRef, DocumentProfileView, InsightCardAction, InsightCardItem, InsightCardV1,
    InsightCardsRequest, InsightConfidence, InsightGeneratedBy, InsightItemKind, PageRange,
    ProfileSection, DOCUMENT_PROFILE_SCHEMA_VERSION, INSIGHT_CARD_SCHEMA_VERSION,
};

pub const CARD_INSTRUCTION_VERSION: &str = "insight-card.v2";
const MAX_CARD_ITEMS: usize = 6;
const MAX_QUOTE_CHARS: usize = 400;
const MAX_ITEM_TEXT_CHARS: usize = 600;

pub struct CardService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    documents: Arc<DocumentService>,
    parse: Arc<ParseIndexService>,
    generation: Option<Arc<dyn StructuredGenerationPort>>,
}

struct ProfileBundle {
    profile: DocumentProfileView,
    blocks: Vec<ContentBlockView>,
}

impl CardService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        documents: Arc<DocumentService>,
        parse: Arc<ParseIndexService>,
        generation: Option<Arc<dyn StructuredGenerationPort>>,
    ) -> Self {
        Self {
            db,
            clock,
            documents,
            parse,
            generation,
        }
    }

    pub fn get_cards(
        &self,
        request: &InsightCardsRequest,
        force: bool,
    ) -> Result<Vec<InsightCardV1>, AppError> {
        let bundle = self.load_profile(request)?;
        let locale = request.locale.clone().unwrap_or_else(|| "zh-CN".into());
        let kinds = if request.kinds.is_empty() {
            vec!["question_starters".to_string()]
        } else {
            request.kinds.clone()
        };
        if kinds.len() > 4 {
            return Err(invalid("too_many_card_kinds"));
        }
        let mut out = Vec::new();
        for kind in kinds {
            if !is_supported_kind(&kind) {
                return Err(invalid("unsupported_card_kind"));
            }
            let card = self.get_one(&bundle, request, &kind, &locale, force)?;
            if let Some(card) = card {
                out.push(card);
            }
        }
        Ok(out)
    }

    pub fn get_profile(
        &self,
        document_id: &str,
        version_id: &str,
    ) -> Result<DocumentProfileView, AppError> {
        let request = InsightCardsRequest {
            document_id: document_id.to_string(),
            version_id: version_id.to_string(),
            page: 0,
            locale: None,
            kinds: Vec::new(),
            heading_path: None,
            block_ids: None,
        };
        Ok(self.load_profile(&request)?.profile)
    }

    fn load_profile(&self, request: &InsightCardsRequest) -> Result<ProfileBundle, AppError> {
        let detail = self.documents.get_document(&request.document_id)?;
        let current = detail.summary.current_version_id.as_deref();
        if current != Some(request.version_id.as_str()) {
            return Err(invalid("stale_document_version"));
        }
        let format = detail.summary.format;
        let status = self.parse.get_status(&request.document_id)?;
        let Some(generation_id) = status
            .generation_id
            .clone()
            .filter(|id| status.version_id == request.version_id && !id.trim().is_empty())
        else {
            return Err(unavailable("parse_not_ready"));
        };
        let blocks = self.parse.list_generation_blocks(&generation_id)?;
        let generation = self.parse.get_generation(&generation_id)?;
        let block_set_hash = generation
            .as_ref()
            .and_then(|g| g.block_set_hash.clone())
            .unwrap_or_else(|| format!("{:?}", generation.as_ref().map(|g| g.status)));
        let profile_version = sha256_str(&format!(
            "{}:{}:{}:{}",
            request.version_id,
            generation_id,
            blocks.len(),
            block_set_hash
        ));
        let profile = build_profile(
            &request.document_id,
            &request.version_id,
            &generation_id,
            format,
            &profile_version,
            &blocks,
        );
        self.save_profile(&profile)?;
        Ok(ProfileBundle { profile, blocks })
    }

    fn get_one(
        &self,
        bundle: &ProfileBundle,
        request: &InsightCardsRequest,
        kind: &str,
        locale: &str,
        force: bool,
    ) -> Result<Option<InsightCardV1>, AppError> {
        let profile_version = bundle.profile.profile_version.clone();
        let scope = scope_key(request);
        if force {
            self.delete_cached(&request.version_id, &profile_version, &scope, locale, kind)?;
        } else if let Some(cached) =
            self.load_cached(&request.version_id, &profile_version, &scope, locale, kind)?
        {
            // 缓存升级:早期版本在“卡片没有任务作用域、被网关拦下”时
            // 存的是抽取式卡片。模型现在可用时,自动重新生成一次 AI 卡片,
            // 避免用户一直看到模板化的旧内容(失败仍回落到抽取式)。
            let upgrade = cached.generated_by == InsightGeneratedBy::Extractive
                && self.ai_available(&request.document_id);
            if !upgrade {
                return Ok(Some(cached));
            }
        }
        let selected = select_blocks(&bundle.blocks, request);
        if selected.is_empty() {
            return Ok(None);
        }
        let now = self.clock.now_ms();
        let card = self.ai_or_extractive(bundle, request, kind, locale, &selected, now)?;
        self.save_card(
            &card,
            request,
            locale,
            &bundle.profile.profile_version,
            &scope,
        )?;
        Ok(Some(card))
    }

    /// 模型是否可用于卡片生成(与 try_ai_card 的前置条件一致)。
    fn ai_available(&self, document_id: &str) -> bool {
        let Some(generation) = self.generation.as_ref() else {
            return false;
        };
        generation
            .capability(document_id)
            .map(|cap| cap.generation_available && cap.generation_authorized)
            .unwrap_or(false)
    }

    fn ai_or_extractive(
        &self,
        bundle: &ProfileBundle,
        request: &InsightCardsRequest,
        kind: &str,
        locale: &str,
        selected: &[&ContentBlockView],
        now: i64,
    ) -> Result<InsightCardV1, AppError> {
        if let Some(card) = self.try_ai_card(bundle, request, kind, locale, selected, now) {
            return Ok(card);
        }
        Ok(build_extractive_card(
            bundle, request, kind, locale, selected, now,
        ))
    }

    fn try_ai_card(
        &self,
        bundle: &ProfileBundle,
        request: &InsightCardsRequest,
        kind: &str,
        locale: &str,
        selected: &[&ContentBlockView],
        now: i64,
    ) -> Option<InsightCardV1> {
        let generation = self.generation.as_ref()?;
        let capability = generation.capability(&request.document_id).ok()?;
        if !capability.generation_available || !capability.generation_authorized {
            return None;
        }
        let block_ids: HashMap<&str, &ContentBlockView> = selected
            .iter()
            .copied()
            .map(|b| (b.id.as_str(), b))
            .collect();
        let inputs = selected
            .iter()
            .take(8)
            .map(|block| GenerationInput {
                source_id: block.id.clone(),
                role: SourceRole::Original,
                text: block.text.chars().take(1500).collect(),
            })
            .collect::<Vec<_>>();
        let question = format!(
            "Generate 3-5 natural, specific questions a careful reader would ask about this {} \
             page/section. Card kind: `{kind}`. Every item MUST be kind ai_inference and MUST \
             include sourceRefs with a blockId from the SOURCE ids plus a short verbatim quote \
             from that block. Questions must be answerable from the SOURCE, non-repetitive, and \
             must not invent facts. Locale: {locale}.",
            bundle.profile.document_type
        );
        let req = StructuredGenerationRequest {
            task_id: format!("card:{}:{}:{kind}", request.version_id, request.page),
            attempt_id: String::new(),
            document_id: request.document_id.clone(),
            snapshot_id: String::new(),
            schema_version: INSIGHT_CARD_SCHEMA_VERSION.to_string(),
            schema: InsightCardV1::ai_submission_schema(),
            instruction_version: CARD_INSTRUCTION_VERSION.to_string(),
            task_kind: TaskKind::InsightCard,
            inputs,
            locale: locale.to_string(),
            question,
            history: Vec::new(),
            history_summary: None,
            attachments: Vec::new(),
            document_map: None,
            // 卡片由 UI 直接触发,没有 Task;使用文档作用域,
            // 授权/载荷哈希/来源白名单/密钥/审计仍全部执行。
            scope: crate::model_privacy::gateway::EgressScope::Document,
        };
        let result = generation
            .generate_streamed(
                &req,
                &tokio_util::sync::CancellationToken::new(),
                &noop_observer(),
            )
            .ok()?;
        validate_ai_card(
            &result.content,
            kind,
            bundle,
            request,
            selected,
            locale,
            &block_ids,
            now,
        )
    }

    fn load_cached(
        &self,
        version_id: &str,
        profile_version: &str,
        scope_key: &str,
        locale: &str,
        kind: &str,
    ) -> Result<Option<InsightCardV1>, AppError> {
        let key = (
            version_id.to_string(),
            profile_version.to_string(),
            scope_key.to_string(),
            locale.to_string(),
            kind.to_string(),
        );
        let json: Option<String> = self.db.read(move |conn| {
            conn.query_row(
                "SELECT card_json FROM insight_card
                 WHERE document_version_id=?1 AND profile_version=?2 AND scope_key=?3
                   AND locale=?4 AND kind=?5",
                params![key.0, key.1, key.2, key.3, key.4],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| db_app_error(&e))
        })?;
        Ok(json.and_then(|json| serde_json::from_str(&json).ok()))
    }

    fn save_card(
        &self,
        card: &InsightCardV1,
        request: &InsightCardsRequest,
        locale: &str,
        profile_version: &str,
        scope_key: &str,
    ) -> Result<(), AppError> {
        let profile_version = profile_version.to_string();
        let scope_key = scope_key.to_string();
        let json = serde_json::to_string(card).map_err(|e| AppError::internal(e.to_string()))?;
        let id = card.id.clone();
        let document_id = request.document_id.clone();
        let version_id = request.version_id.clone();
        let page = request.page;
        let locale = locale.to_string();
        let kind = card.kind.clone();
        let generated_by = match card.generated_by {
            InsightGeneratedBy::Ai => "ai",
            InsightGeneratedBy::Extractive => "extractive",
        }
        .to_string();
        let confidence = match card.confidence {
            InsightConfidence::High => "high",
            InsightConfidence::Medium => "medium",
            InsightConfidence::Low => "low",
        }
        .to_string();
        let now = self.clock.now_ms();
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO insight_card
                    (id, document_id, document_version_id, profile_version, page_number,
                     scope_key, locale, kind, generated_by, confidence, card_json,
                     created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?12)
                 ON CONFLICT(document_version_id, profile_version, scope_key, locale, kind)
                 DO UPDATE SET card_json=excluded.card_json, generated_by=excluded.generated_by,
                               confidence=excluded.confidence, updated_at=excluded.updated_at",
                params![
                    id,
                    document_id,
                    version_id,
                    profile_version,
                    page,
                    scope_key,
                    locale,
                    kind,
                    generated_by,
                    confidence,
                    json,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    fn delete_cached(
        &self,
        version_id: &str,
        profile_version: &str,
        scope_key: &str,
        locale: &str,
        kind: &str,
    ) -> Result<(), AppError> {
        let key = (
            version_id.to_string(),
            profile_version.to_string(),
            scope_key.to_string(),
            locale.to_string(),
            kind.to_string(),
        );
        self.db.write(move |conn| {
            conn.execute(
                "DELETE FROM insight_card
                 WHERE document_version_id=?1 AND profile_version=?2 AND scope_key=?3
                   AND locale=?4 AND kind=?5",
                params![key.0, key.1, key.2, key.3, key.4],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    fn save_profile(&self, profile: &DocumentProfileView) -> Result<(), AppError> {
        let title_tree = serde_json::to_string(&profile.title_tree)
            .map_err(|e| AppError::internal(e.to_string()))?;
        let id = deterministic_id("profile", &profile.profile_version);
        let p = profile.clone();
        let now = self.clock.now_ms();
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO document_profile
                    (id, document_id, document_version_id, parse_generation_id, profile_version,
                     format, language, document_type, page_count, section_count,
                     title_tree_json, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
                 ON CONFLICT(document_version_id, profile_version) DO UPDATE SET
                    title_tree_json=excluded.title_tree_json,
                    section_count=excluded.section_count,
                    page_count=excluded.page_count",
                params![
                    id,
                    p.document_id,
                    p.document_version_id,
                    p.parse_generation_id,
                    p.profile_version,
                    p.format,
                    p.language,
                    p.document_type,
                    p.page_count,
                    p.section_count,
                    title_tree,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }
}

fn deterministic_id(prefix: &str, seed: &str) -> String {
    format!("{prefix}_{}", sha256_str(seed))
}

fn invalid(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, "insights").with_detail("reason", reason.into())
}

fn unavailable(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, "insights").with_detail("reason", reason.into())
}

fn scope_key(request: &InsightCardsRequest) -> String {
    if let Some(path) = &request.heading_path {
        if !path.is_empty() {
            return format!("section:{}", sha256_str(&path.join("/")));
        }
    }
    if let Some(ids) = &request.block_ids {
        if !ids.is_empty() {
            return format!("blocks:{}", sha256_str(&ids.join("|")));
        }
    }
    format!("page:{}", request.page)
}

fn display_heading(title: &str) -> String {
    title.split('#').next().unwrap_or(title).trim().to_string()
}

fn is_supported_kind(kind: &str) -> bool {
    kind == "question_starters"
}

fn select_blocks<'a>(
    blocks: &'a [ContentBlockView],
    request: &InsightCardsRequest,
) -> Vec<&'a ContentBlockView> {
    if let Some(ids) = &request.block_ids {
        let wanted: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
        let selected: Vec<_> = blocks
            .iter()
            .filter(|b| wanted.contains(b.id.as_str()))
            .collect();
        if !selected.is_empty() {
            return selected;
        }
    }
    if let Some(path) = &request.heading_path {
        if !path.is_empty() {
            let selected: Vec<_> = blocks
                .iter()
                .filter(|b| {
                    b.heading_path.len() >= path.len() && b.heading_path[..path.len()] == path[..]
                })
                .collect();
            if !selected.is_empty() {
                return selected;
            }
        }
    }
    if request.page > 0 {
        let selected: Vec<_> = blocks
            .iter()
            .filter(|b| b.locator.pdf_page() == Some(request.page))
            .collect();
        if !selected.is_empty() {
            return selected;
        }
    }
    blocks
        .iter()
        .filter(|b| !b.text.trim().is_empty())
        .take(8)
        .collect()
}

fn build_profile(
    document_id: &str,
    version_id: &str,
    generation_id: &str,
    format: DocumentFormat,
    profile_version: &str,
    blocks: &[ContentBlockView],
) -> DocumentProfileView {
    let text_sample: String = blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(40_000)
        .collect();
    let headings: Vec<&ContentBlockView> = blocks
        .iter()
        .filter(|b| {
            b.kind == crate::parse_index::models::BlockKind::Heading && !b.heading_path.is_empty()
        })
        .collect();
    let section_count = headings.len() as i64;
    let page_count = blocks.iter().filter_map(|b| b.locator.pdf_page()).max();
    let title_tree = headings
        .iter()
        .take(80)
        .map(|heading| {
            let path = heading.heading_path.clone();
            let pages: Vec<i64> = blocks
                .iter()
                .filter(|b| {
                    b.heading_path.len() >= path.len() && b.heading_path[..path.len()] == path[..]
                })
                .filter_map(|b| b.locator.pdf_page())
                .collect();
            ProfileSection {
                title: path
                    .last()
                    .map(|title| display_heading(title))
                    .unwrap_or_else(|| display_heading(&heading.text)),
                heading_path: path,
                page_start: pages.iter().min().copied(),
                page_end: pages.iter().max().copied(),
            }
        })
        .collect();
    DocumentProfileView {
        schema_version: DOCUMENT_PROFILE_SCHEMA_VERSION.to_string(),
        document_id: document_id.to_string(),
        document_version_id: version_id.to_string(),
        parse_generation_id: Some(generation_id.to_string()),
        profile_version: profile_version.to_string(),
        format: format.as_str().to_string(),
        language: detect_language(&text_sample).to_string(),
        document_type: detect_document_type(&text_sample).to_string(),
        page_count,
        section_count,
        title_tree,
    }
}

/// 抽取式卡片的纯构建函数(可单测):不访问数据库、不编造正文。
fn build_extractive_card(
    bundle: &ProfileBundle,
    request: &InsightCardsRequest,
    kind: &str,
    _locale: &str,
    selected: &[&ContentBlockView],
    now: i64,
) -> InsightCardV1 {
    // 抽取式降级只生成可追溯到原文的“猜你想问”；正文不是模型结论。
    let mut seen = HashSet::new();
    let items: Vec<InsightCardItem> = selected
        .iter()
        .copied()
        .filter(|b| !b.text.trim().is_empty())
        .filter_map(|block| {
            let heading = block
                .heading_path
                .last()
                .map(|title| display_heading(title))
                .unwrap_or_default();
            let question = if heading.is_empty() {
                let snippet = primary_text(block);
                format!(
                    "「{}」的核心结论是什么？",
                    snippet.chars().take(40).collect::<String>()
                )
            } else {
                format!("围绕「{heading}」这一节，核心结论是什么？")
            };
            if !seen.insert(question.clone()) {
                return None;
            }
            Some(InsightCardItem {
                text: question,
                kind: InsightItemKind::AiInference,
                source_refs: vec![source_ref(block, &primary_text(block))],
                citation_ids: Vec::new(),
            })
        })
        .take(4)
        .collect();
    let items = items.into_iter().take(MAX_CARD_ITEMS).collect::<Vec<_>>();
    let title = card_title();
    let actions = card_actions(&items);
    let source_page_range = page_range(selected);
    InsightCardV1 {
        schema_version: INSIGHT_CARD_SCHEMA_VERSION.to_string(),
        id: format!(
            "card_{}",
            sha256_str(&format!(
                "{}:{}:{}:{}:{}:{}",
                bundle.profile.profile_version,
                request.version_id,
                request.page,
                kind,
                request
                    .heading_path
                    .as_ref()
                    .map(|p| p.join("/"))
                    .unwrap_or_default(),
                title
            ))
        ),
        kind: kind.to_string(),
        title,
        subtitle: None,
        items,
        actions,
        source_page_range,
        generated_by: InsightGeneratedBy::Extractive,
        generated_at: now,
        confidence: InsightConfidence::Medium,
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_ai_card(
    value: &serde_json::Value,
    kind: &str,
    bundle: &ProfileBundle,
    request: &InsightCardsRequest,
    selected: &[&ContentBlockView],
    locale: &str,
    block_ids: &HashMap<&str, &ContentBlockView>,
    now: i64,
) -> Option<InsightCardV1> {
    let title = value.get("title")?.as_str()?.trim().to_string();
    if title.is_empty() || title.chars().count() > 120 {
        return None;
    }
    let subtitle = value
        .get("subtitle")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let raw_items = value.get("items")?.as_array()?;
    let mut items = Vec::new();
    for raw in raw_items.iter().take(MAX_CARD_ITEMS) {
        let text =
            crate::shared::sanitize::sanitize_generated_markdown(raw.get("text")?.as_str()?.trim());
        if text.is_empty() || text.chars().count() > MAX_ITEM_TEXT_CHARS {
            continue;
        }
        let kind_value = raw
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("ai_inference");
        let item_kind = if kind_value == "document_fact" {
            InsightItemKind::DocumentFact
        } else {
            InsightItemKind::AiInference
        };
        let mut refs = Vec::new();
        if let Some(refs_raw) = raw.get("sourceRefs").and_then(|v| v.as_array()) {
            for reference in refs_raw.iter().take(4) {
                let Some(block_id) = reference.get("blockId").and_then(|v| v.as_str()) else {
                    continue;
                };
                let Some(block) = block_ids.get(block_id) else {
                    continue;
                };
                let quote = reference
                    .get("quote")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .chars()
                    .take(MAX_QUOTE_CHARS)
                    .collect::<String>();
                if quote.chars().count() < 2 || !contains_normalized(&block.text, &quote) {
                    continue;
                }
                refs.push(source_ref(block, &quote));
            }
        }
        if refs.is_empty() {
            // 猜你想问也必须可追溯;无有效来源的条目直接剔除。
            continue;
        }
        items.push(InsightCardItem {
            text,
            kind: item_kind,
            source_refs: refs,
            citation_ids: Vec::new(),
        });
    }
    if items.is_empty() {
        return None;
    }
    let actions = card_actions(&items);
    let source_page_range = page_range(selected);
    let _ = locale;
    let mut card = InsightCardV1 {
        schema_version: INSIGHT_CARD_SCHEMA_VERSION.to_string(),
        id: String::new(),
        kind: kind.to_string(),
        title,
        subtitle,
        items,
        actions,
        source_page_range,
        generated_by: InsightGeneratedBy::Ai,
        generated_at: now,
        confidence: InsightConfidence::Medium,
    };
    // 统一 id 规则,保证缓存键稳定。
    card.id = format!(
        "card_{}",
        sha256_str(&format!(
            "{}:{}:{}:{}:{}:{}",
            bundle.profile.profile_version,
            request.version_id,
            request.page,
            kind,
            request
                .heading_path
                .as_ref()
                .map(|p| p.join("/"))
                .unwrap_or_default(),
            card.title
        ))
    );
    Some(card)
}

fn contains_normalized(haystack: &str, needle: &str) -> bool {
    let normalize = |text: &str| {
        text.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    normalize(haystack).contains(&normalize(needle))
}

fn primary_text(block: &ContentBlockView) -> String {
    let text = block.text.trim();
    if text.is_empty() {
        return block.heading_path.last().cloned().unwrap_or_default();
    }
    // 第一句(按中英文句末/换行);表格取首行。必须是原文子串。
    let mut out = String::new();
    for ch in text.chars() {
        if out.chars().count() >= 220 {
            break;
        }
        out.push(ch);
        if matches!(ch, '。' | '！' | '？' | '!' | '?' | '\n') {
            break;
        }
        if ch == '.' {
            // 英文句点仅在后接空白/结束处断句,避免 1.5/3.2 误切。
            if out.chars().count() >= 40 {
                break;
            }
        }
    }
    out.trim().to_string()
}

fn source_ref(block: &ContentBlockView, quote: &str) -> CardSourceRef {
    CardSourceRef {
        block_id: block.id.clone(),
        page: block.locator.pdf_page(),
        heading_path: block.heading_path.clone(),
        quote: quote.chars().take(MAX_QUOTE_CHARS).collect(),
        locator: Some(block.locator.clone()),
    }
}

fn page_range(selected: &[&ContentBlockView]) -> Option<PageRange> {
    let pages: Vec<i64> = selected
        .iter()
        .filter_map(|b| b.locator.pdf_page())
        .collect();
    Some(PageRange {
        start: *pages.iter().min()?,
        end: *pages.iter().max()?,
    })
}

fn card_title() -> String {
    "猜你想问".to_string()
}

fn card_actions(items: &[InsightCardItem]) -> Vec<InsightCardAction> {
    let first = items.first();
    let mut actions = Vec::new();
    if let Some(item) = first {
        let mut args = BTreeMap::new();
        args.insert("prompt".to_string(), item.text.clone());
        actions.push(InsightCardAction {
            id: "ask".to_string(),
            label: "追问".to_string(),
            tool: "ask".to_string(),
            args,
        });
        if let Some(source) = item.source_refs.first() {
            let mut args = BTreeMap::new();
            args.insert("blockId".to_string(), source.block_id.clone());
            if let Some(locator) = &source.locator {
                if let Ok(json) = serde_json::to_string(locator) {
                    args.insert("locator".to_string(), json);
                }
            }
            actions.push(InsightCardAction {
                id: "show_evidence".to_string(),
                label: "看原文".to_string(),
                tool: "show_evidence".to_string(),
                args,
            });
        }
    }
    actions
}

fn detect_language(text: &str) -> &'static str {
    let mut cjk = 0usize;
    let mut latin = 0usize;
    for ch in text.chars() {
        if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            cjk += 1;
        } else if ch.is_ascii_alphabetic() {
            latin += 1;
        }
    }
    if cjk == 0 && latin == 0 {
        "unknown"
    } else if cjk >= latin / 4 {
        "zh"
    } else if latin >= cjk / 4 {
        "en"
    } else {
        "mixed"
    }
}

fn detect_document_type(text: &str) -> &'static str {
    let sample = text.chars().take(20_000).collect::<String>().to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| sample.contains(n));
    if has(&["合同", "甲方", "乙方", "违约", "协议"]) {
        "contract"
    } else if has(&["手册", "操作步骤", "注意事项", "警告", "安装"]) {
        "manual"
    } else if has(&["论文", "摘要", "研究方法", "实验", "参考文献"]) {
        "paper"
    } else if has(&["教材", "习题", "复习", "章节", "课堂"]) {
        "textbook"
    } else if has(&["报告", "结论", "建议", "指标", "数据来源"]) {
        "report"
    } else {
        "general"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::detect::SizeLimits;
    use crate::document::DocumentService;
    use crate::parse_index::models::{BlockCompleteness, BlockKind};
    use crate::parse_index::ParseIndexService;
    use crate::persistence::task::TaskService;
    use crate::persistence::Db;
    use crate::retrieval_ai::ports::ModelCapabilityView;
    use crate::shared::clock::TestClock;
    use crate::shared::events::CollectingEventSink;
    use crate::shared::ids::TestIdGenerator;
    use crate::shared::locator::{PageRotation, SourceLocator};
    use std::sync::Mutex;

    fn block(id: &str, text: &str, path: &[&str], page: i64) -> ContentBlockView {
        ContentBlockView {
            id: id.to_string(),
            document_version_id: "v1".into(),
            parse_generation_id: Some("g1".into()),
            origin: crate::parse_index::models::BlockOrigin::Parser,
            block_key: id.into(),
            ordinal: Some(page),
            kind: BlockKind::Paragraph,
            text: text.into(),
            heading_path: path.iter().map(|s| s.to_string()).collect(),
            locator: SourceLocator::pdf(page, vec![], PageRotation::R0),
            completeness: BlockCompleteness::Complete,
            indexable: true,
            ocr_confidence: None,
        }
    }

    fn bundle(blocks: Vec<ContentBlockView>) -> ProfileBundle {
        let profile = DocumentProfileView {
            schema_version: DOCUMENT_PROFILE_SCHEMA_VERSION.into(),
            document_id: "d1".into(),
            document_version_id: "v1".into(),
            parse_generation_id: Some("g1".into()),
            profile_version: "pv1".into(),
            format: "pdf".into(),
            language: "zh".into(),
            document_type: "general".into(),
            page_count: Some(3),
            section_count: 1,
            title_tree: vec![],
        };
        ProfileBundle { profile, blocks }
    }

    fn request() -> InsightCardsRequest {
        InsightCardsRequest {
            document_id: "d1".into(),
            version_id: "v1".into(),
            page: 2,
            locale: Some("zh-CN".into()),
            kinds: vec![],
            heading_path: None,
            block_ids: None,
        }
    }

    #[test]
    fn extractive_question_card_has_locatable_source_and_no_duplicates() {
        let blocks = vec![
            block("b1", "第一句原文。第二句。", &["第一章"], 2),
            block("b2", "另一段原文。", &["第一章"], 2),
            block("b3", "第二章的内容。", &["第二章"], 2),
        ];
        let b = bundle(blocks);
        let selected: Vec<&ContentBlockView> = b.blocks.iter().collect();
        let card =
            build_extractive_card(&b, &request(), "question_starters", "zh-CN", &selected, 0);
        assert_eq!(card.generated_by, InsightGeneratedBy::Extractive);
        assert_eq!(card.title, "猜你想问");
        assert!(!card.items.is_empty());
        let mut seen = HashSet::new();
        for item in &card.items {
            assert_eq!(item.kind, InsightItemKind::AiInference);
            assert!(seen.insert(item.text.clone()), "问题不得重复");
            let source = item.source_refs.first().expect("问题必须有来源");
            let block = b.blocks.iter().find(|b| b.id == source.block_id).unwrap();
            assert!(block.text.contains(&source.quote));
            assert_eq!(source.page, Some(2));
        }
        assert!(card.actions.iter().any(|a| a.tool == "ask"));
        assert!(card.actions.iter().any(|a| a.tool == "show_evidence"));
    }

    #[test]
    fn ai_question_card_requires_traceable_source() {
        let blocks = vec![block("b1", "真实原文。", &["第一章"], 2)];
        let b = bundle(blocks);
        let selected = b.blocks.iter().collect::<Vec<_>>();
        let map: HashMap<&str, &ContentBlockView> =
            b.blocks.iter().map(|b| (b.id.as_str(), b)).collect();
        let value = serde_json::json!({
            "schemaVersion": "insight_card.v1",
            "title": "猜你想问",
            "items": [
                {"text": "这一节的核心结论是什么？", "kind": "ai_inference", "sourceRefs": [{"blockId": "b1", "quote": "真实原文"}]},
                {"text": "没有来源的问题？", "kind": "ai_inference", "sourceRefs": []}
            ]
        });
        let card = validate_ai_card(
            &value,
            "question_starters",
            &b,
            &request(),
            &selected,
            "zh-CN",
            &map,
            0,
        )
        .expect("有效问题应通过校验");
        assert_eq!(card.generated_by, InsightGeneratedBy::Ai);
        assert_eq!(card.items.len(), 1);
        assert_eq!(card.items[0].source_refs[0].block_id, "b1");
    }

    #[test]
    fn ai_submission_with_invented_fact_is_rejected() {
        let blocks = vec![block("b1", "真实原文。", &["第一章"], 2)];
        let b = bundle(blocks);
        let selected = b.blocks.iter().collect::<Vec<_>>();
        let map: HashMap<&str, &ContentBlockView> =
            b.blocks.iter().map(|b| (b.id.as_str(), b)).collect();
        let value = serde_json::json!({
            "schemaVersion": "insight_card.v1",
            "title": "测试",
            "items": [
                {"text": "编造事实", "kind": "document_fact", "sourceRefs": [{"blockId": "b1", "quote": "不存在的原文"}]}
            ]
        });
        let card = validate_ai_card(
            &value,
            "question_starters",
            &b,
            &request(),
            &selected,
            "zh-CN",
            &map,
            0,
        );
        assert!(card.is_none(), "无有效来源的条目必须整体剔除后降级");
    }

    struct FakeCardPort {
        content: serde_json::Value,
        prompts: Mutex<Vec<String>>,
    }

    impl StructuredGenerationPort for FakeCardPort {
        fn capability(&self, _document_id: &str) -> Result<ModelCapabilityView, AppError> {
            Ok(ModelCapabilityView {
                generation_available: true,
                provider_id: Some("fake".into()),
                model_id: Some("fake-model".into()),
                generation_authorized: true,
            })
        }

        fn generate(
            &self,
            request: &StructuredGenerationRequest,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<crate::retrieval_ai::models::StructuredGenerationResult, AppError> {
            self.prompts.lock().unwrap().push(request.question.clone());
            Ok(crate::retrieval_ai::models::StructuredGenerationResult {
                content: self.content.clone(),
                model_provider_id: "fake".into(),
                model_id: "fake-model".into(),
                partial: false,
            })
        }
    }

    fn card_service_with(
        generation: Option<Arc<dyn StructuredGenerationPort>>,
    ) -> (CardService, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "kw-card-ai-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Arc::new(Db::open(&dir.join("t.sqlite3"), 2).unwrap());
        let clock: Arc<dyn Clock> = Arc::new(TestClock::new(1_000_000));
        let ids = Arc::new(TestIdGenerator::new());
        let events = Arc::new(CollectingEventSink::default());
        let tasks = Arc::new(TaskService::new(
            Arc::clone(&db),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let documents = Arc::new(DocumentService::new(
            Arc::clone(&db),
            clock.clone(),
            ids.clone(),
            events.clone(),
            SizeLimits::default(),
        ));
        let parse = Arc::new(ParseIndexService::new(
            Arc::clone(&db),
            clock.clone(),
            ids,
            events,
            Arc::clone(&tasks),
        ));
        (
            CardService::new(db, clock, documents, parse, generation),
            dir,
        )
    }

    #[test]
    fn question_starters_are_generated_by_ai_when_model_available() {
        let fake = Arc::new(FakeCardPort {
            content: serde_json::json!({
                "schemaVersion": "insight_card.v1",
                "title": "猜你想问",
                "items": [
                    {
                        "text": "预算总额与研发占比之间是什么关系？",
                        "kind": "ai_inference",
                        "sourceRefs": [{"blockId": "b1", "quote": "总预算为一百二十万元"}]
                    }
                ]
            }),
            prompts: Mutex::new(Vec::new()),
        });
        let (service, dir) = card_service_with(Some(fake.clone()));
        let b = bundle(vec![block(
            "b1",
            "总预算为一百二十万元，其中研发投入占比最高。",
            &["第二章"],
            2,
        )]);
        let selected = b.blocks.iter().collect::<Vec<_>>();
        let card = service
            .try_ai_card(&b, &request(), "question_starters", "zh-CN", &selected, 0)
            .expect("模型可用时必须走 AI 卡片");
        assert_eq!(card.generated_by, InsightGeneratedBy::Ai);
        assert_eq!(card.items[0].text, "预算总额与研发占比之间是什么关系？");
        assert_eq!(card.items[0].source_refs[0].block_id, "b1");
        let prompts = fake.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("Generate 3-5 natural"));
        assert!(prompts[0].contains("question_starters"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn profile_version_changes_with_generation_content() {
        let a = sha256_str("v1:g1:10:hash-a");
        let b = sha256_str("v1:g2:10:hash-b");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_scope_distinguishes_pages_and_sections() {
        let mut page = request();
        page.page = 3;
        let mut section = request();
        section.page = 0;
        section.heading_path = Some(vec!["第一章".into(), "第二节".into()]);
        let mut other_section = request();
        other_section.page = 0;
        other_section.heading_path = Some(vec!["第一章".into(), "第三节".into()]);
        assert_eq!(scope_key(&page), "page:3");
        assert_ne!(scope_key(&section), scope_key(&other_section));
        assert_ne!(scope_key(&page), scope_key(&section));
    }

    #[test]
    fn document_type_and_language_are_derived_from_content() {
        assert_eq!(detect_document_type("甲方与乙方签订本合同"), "contract");
        assert_eq!(detect_document_type("实验方法与参考文献"), "paper");
        assert_eq!(detect_language("这是中文"), "zh");
        assert_eq!(detect_language("this is english"), "en");
    }
}

/// 2G 隐私清除:动态卡片/画像缓存(按文档,可重建)。
pub struct CardsClearContributor {
    db: Arc<Db>,
}

impl CardsClearContributor {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

impl crate::model_privacy::clear::DataClearContributor for CardsClearContributor {
    fn contributor_id(&self) -> &'static str {
        "m13_insight_cards"
    }
    fn supported_categories(&self) -> &'static [crate::model_privacy::models::ClearCategory] {
        &[crate::model_privacy::models::ClearCategory::InsightCards]
    }
    fn preview(
        &self,
        document_id: &str,
        categories: &[crate::model_privacy::models::ClearCategory],
    ) -> Result<Vec<crate::model_privacy::models::ClearCategoryPreview>, AppError> {
        use crate::model_privacy::models::{ClearCategory, ClearCategoryPreview};
        if !categories.contains(&ClearCategory::InsightCards) {
            return Ok(Vec::new());
        }
        let document_id = document_id.to_string();
        let count: i64 = self.db.read(move |conn| {
            conn.query_row(
                "SELECT (SELECT COUNT(*) FROM insight_card WHERE document_id=?1) +
                        (SELECT COUNT(*) FROM document_profile WHERE document_id=?1)",
                [document_id],
                |row| row.get(0),
            )
            .map_err(|e| db_app_error(&e))
        })?;
        Ok(vec![ClearCategoryPreview {
            category: ClearCategory::InsightCards,
            available: true,
            unavailable_reason: None,
            count: Some(count),
            affected_citations: Some(0),
            protected_count: None,
        }])
    }
    fn execute(
        &self,
        document_id: &str,
        categories: &[crate::model_privacy::models::ClearCategory],
    ) -> Result<Vec<crate::model_privacy::models::ClearCategoryExecution>, AppError> {
        use crate::model_privacy::models::{ClearCategory, ClearCategoryExecution};
        if !categories.contains(&ClearCategory::InsightCards) {
            return Ok(Vec::new());
        }
        let document_id = document_id.to_string();
        let deleted: i64 = self.db.write(move |conn| {
            let cards = conn
                .execute(
                    "DELETE FROM insight_card WHERE document_id=?1",
                    [&document_id],
                )
                .map_err(|e| db_app_error(&e))?;
            let profiles = conn
                .execute(
                    "DELETE FROM document_profile WHERE document_id=?1",
                    [&document_id],
                )
                .map_err(|e| db_app_error(&e))?;
            Ok((cards + profiles) as i64)
        })?;
        Ok(vec![ClearCategoryExecution {
            category: ClearCategory::InsightCards,
            deleted,
        }])
    }
}
