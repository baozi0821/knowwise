//! M05 锚点与证据服务(《M05》§2–§9)。
//!
//! 关键不变量:
//! - M05 独占 Anchor/Citation 校验与证据保留裁决;不渲染文档、不保存回答正文。
//! - 结构校验与语义支持分离:`structuralStatus=valid` 不等于 `supportStatus=supported`;
//!   模型自检只能写 `supportSource=model`,用户确认才可写 `supportSource=user`。
//! - 引文必须是单一 ContentBlock 的连续原始片段:精确匹配权威,规范化匹配
//!   只处理换行/软连字符/Unicode 兼容差异,保存值仍是原文切片。
//! - 文档事实 Citation 只能以 ContentBlock / Anchor 为权威证据;AI 草稿不得
//!   成为 document_fact 的唯一支持(整批拒绝,M09 负责降级)。
//! - Anchor 版本落后于文档当前版本 ⇒ 有效状态 orphaned:旧引用默认打开快照,
//!   用户确认重定位前不自动绑定新版本。
//! - 重定位候选只是 proposed;confirmed 才建立新 Anchor,旧 Anchor 历史保持。
//! - 批量 Citation 超限拒绝整段结果,不截断证据、不显示部分已核验。
//!
//! 跨模块读取说明:本模块对 `document`/`document_version`(M02)与
//! `content_block`/`parse_generation`(M04)只做单表/显式 JOIN 的只读校验,
//! 与 parse_index 读取 document 表的既有先例一致;写入仍严格限于本模块表。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::{params, Connection, OptionalExtension};

use crate::document::models::DocumentFormat;
use crate::parse_index::models::BLOCK_COLS;
use crate::parse_index::{ParseIndexService, SelectionBlockRequest};
use crate::persistence::{db_app_error, Db};
use crate::shared::locator::SourceLocator;
use crate::shared::textnorm::{normalize_v1, sha256_str};
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::*;

/// 批量引用上限:超限拒绝整段结果,不截断候选(《M05》§9)。
pub const MAX_CITATIONS_PER_BATCH: usize = 64;
/// 单个锚点最多引用的选区块数。
pub const MAX_ANCHOR_BLOCKS: usize = 32;
/// 重定位候选上限(每锚点每目标版本)。
pub const MAX_RELOCATION_CANDIDATES: usize = 5;

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

/// 属主存在性校验器:纯函数指针,在同一事务连接上执行
/// (M09 提交回答时消息与 Citation 同事务,校验必须看到未提交行)。
pub type OwnerCheckFn =
    fn(&Connection, owner_id: &str, document_id: &str) -> Result<bool, AppError>;

/// 已注册的属主校验器集合(Copy,可安全带入写事务)。
#[derive(Default, Clone)]
pub struct OwnerValidators {
    checks: HashMap<CitationOwnerType, OwnerCheckFn>,
}

impl OwnerValidators {
    pub fn register(&mut self, owner_type: CitationOwnerType, check: OwnerCheckFn) {
        self.checks.insert(owner_type, check);
    }

    pub fn get(&self, owner_type: &CitationOwnerType) -> Option<OwnerCheckFn> {
        self.checks.get(owner_type).copied()
    }
}

// —— 跨模块只读校验(M02/M04 表,单表 SELECT)——

fn document_format_of(
    conn: &Connection,
    document_id: &str,
) -> Result<Option<DocumentFormat>, AppError> {
    let fmt: Option<String> = conn
        .query_row(
            "SELECT format FROM document WHERE id=?1",
            [document_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    match fmt {
        None => Ok(None),
        Some(f) => DocumentFormat::parse_db(&f)
            .map(Some)
            .ok_or_else(|| err(CoreErrorCode::Internal)),
    }
}

/// 校验版本归属与指纹状态;返回文档格式。
fn validate_version_tx(
    conn: &Connection,
    document_id: &str,
    version_id: &str,
) -> Result<DocumentFormat, AppError> {
    let format =
        document_format_of(conn, document_id)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT document_id, fingerprint_status FROM document_version WHERE id=?1",
            [version_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    let (owner, fp) = row.ok_or_else(|| err(CoreErrorCode::NotFound))?;
    if owner != document_id {
        return Err(err(CoreErrorCode::NotFound));
    }
    if fp != "ready" {
        return Err(err(CoreErrorCode::IndexGenerationUnavailable)
            .with_detail("reason", "fingerprint_not_ready".into()));
    }
    Ok(format)
}

/// 只读块加载(M04 content_block;evidence 校验用途)。
#[derive(Debug, Clone)]
pub struct BlockRefRow {
    pub id: String,
    pub document_version_id: String,
    pub parse_generation_id: Option<String>,
    pub origin: String,
    pub block_key: String,
    pub ordinal: Option<i64>,
    pub kind: String,
    pub text: String,
    pub heading_path_json: String,
    pub locator_json: String,
    pub indexable: bool,
}

fn load_block_tx(conn: &Connection, block_id: &str) -> Result<Option<BlockRefRow>, AppError> {
    conn.query_row(
        "SELECT id, document_version_id, parse_generation_id, origin, block_key, ordinal, \
         kind, text, heading_path_json, locator_json, indexable FROM content_block WHERE id=?1",
        [block_id],
        |r| {
            Ok(BlockRefRow {
                id: r.get(0)?,
                document_version_id: r.get(1)?,
                parse_generation_id: r.get(2)?,
                origin: r.get(3)?,
                block_key: r.get(4)?,
                ordinal: r.get(5)?,
                kind: r.get(6)?,
                text: r.get(7)?,
                heading_path_json: r.get(8)?,
                locator_json: r.get(9)?,
                indexable: r.get::<_, i64>(10)? != 0,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

/// 当前版本 ID(未确认导入等场景可能为空)。
fn current_version_id_tx(conn: &Connection, document_id: &str) -> Result<Option<String>, AppError> {
    conn.query_row(
        "SELECT current_version_id FROM document WHERE id=?1",
        [document_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

/// 有效锚点状态:invalid 优先;版本落后于当前版本 ⇒ orphaned(《M05》§2/§6)。
pub fn effective_anchor_status(
    stored: AnchorStatus,
    anchor_version_id: &str,
    current_version_id: Option<&str>,
) -> AnchorStatus {
    if stored == AnchorStatus::Invalid {
        return AnchorStatus::Invalid;
    }
    match current_version_id {
        Some(cur) if cur == anchor_version_id => stored,
        _ => AnchorStatus::Orphaned,
    }
}

fn load_anchor_tx(conn: &Connection, anchor_id: &str) -> Result<Option<AnchorRow>, AppError> {
    conn.query_row(
        &format!("SELECT {ANCHOR_COLS} FROM anchor WHERE id=?1"),
        [anchor_id],
        |r| AnchorRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_anchor_by_key_tx(
    conn: &Connection,
    idempotency_key: &str,
) -> Result<Option<AnchorRow>, AppError> {
    conn.query_row(
        &format!("SELECT {ANCHOR_COLS} FROM anchor WHERE idempotency_key=?1"),
        [idempotency_key],
        |r| AnchorRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn load_anchor_blocks_tx(
    conn: &Connection,
    anchor_id: &str,
) -> Result<Vec<AnchorBlockRow>, AppError> {
    let mut stmt = conn
        .prepare(
            "SELECT anchor_id, block_id, position, block_quote_text FROM anchor_block \
             WHERE anchor_id=?1 ORDER BY position",
        )
        .map_err(|e| db_app_error(&e))?;
    let rows = stmt
        .query_map([anchor_id], |r| {
            Ok(AnchorBlockRow {
                anchor_id: r.get(0)?,
                block_id: r.get(1)?,
                position: r.get(2)?,
                block_quote_text: r.get(3)?,
            })
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(rows)
}

/// 组装 AnchorView(含 overlay 投影:块定位 + PDF 页码)。
fn anchor_view_tx(conn: &Connection, row: &AnchorRow) -> Result<AnchorView, AppError> {
    let current = current_version_id_tx(conn, &row.document_id)?;
    let status = effective_anchor_status(row.status, &row.document_version_id, current.as_deref());
    let mut blocks = Vec::new();
    let mut pages = Vec::new();
    for ab in load_anchor_blocks_tx(conn, &row.id)? {
        let locator = load_block_tx(conn, &ab.block_id)?
            .and_then(|b| serde_json::from_str::<SourceLocator>(&b.locator_json).ok());
        if let Some(loc) = &locator {
            if let Some(p) = loc.pdf_page() {
                if !pages.contains(&p) {
                    pages.push(p);
                }
            }
        }
        blocks.push(AnchorBlockView {
            block_id: ab.block_id,
            position: ab.position,
            quote_text: ab.block_quote_text,
            locator: locator.unwrap_or_else(|| {
                // 块被清理等极端场景:回退主定位,不构造无效定位。
                serde_json::from_str(&row.primary_locator_json).unwrap_or(
                    SourceLocator::Composite {
                        parts: vec![],
                        locator_version: 1,
                    },
                )
            }),
        });
    }
    Ok(AnchorView {
        id: row.id.clone(),
        document_id: row.document_id.clone(),
        document_version_id: row.document_version_id.clone(),
        kind: row.kind,
        status,
        exact_text: row.exact_text.clone(),
        snapshot_text: row.snapshot_text.clone(),
        primary_locator: serde_json::from_str(&row.primary_locator_json).unwrap_or(
            SourceLocator::Composite {
                parts: vec![],
                locator_version: 1,
            },
        ),
        blocks,
        overlay_pages: pages,
        created_at: row.created_at,
    })
}

// —— 引文校验算法(《M05》§4)——

/// 校验输出:结构状态 + 命中时的原文切片与 UTF-16 区间。
#[derive(Debug, Clone)]
pub struct CitationValidation {
    pub structural_status: StructuralStatus,
    pub block_id: Option<String>,
    /// 保存值:必须是原文切片(规范化匹配命中时重写为切片)。
    pub saved_quote: Option<String>,
    pub quote_start_utf16: Option<usize>,
    pub quote_end_utf16: Option<usize>,
    pub normalized_match: bool,
}

fn utf16_range_of(hay: &str, start_byte: usize, end_byte: usize) -> (usize, usize) {
    let start = hay[..start_byte].encode_utf16().count();
    let end = start + hay[start_byte..end_byte].encode_utf16().count();
    (start, end)
}

/// 引文匹配结果:(原文切片, 块内 UTF-16 区间, 是否规范化命中)。
type QuoteMatch = (String, Option<(usize, usize)>, bool);

/// 在单一块文本内匹配引文:精确 → 去首尾空白精确 → 规范化(带偏移映射)。
/// 任何路径保存的引文都是原文切片(《M05》§4、A3)。
fn match_quote_in_text(block_text: &str, quoted_text: &str) -> Option<QuoteMatch> {
    if quoted_text.is_empty() {
        return None;
    }
    // 精确子串(权威路径)。
    if let Some(byte) = block_text.find(quoted_text) {
        let (s, e) = utf16_range_of(block_text, byte, byte + quoted_text.len());
        return Some((quoted_text.to_string(), Some((s, e)), false));
    }
    // 引文允许省略块首尾空白:匹配裁剪后的引文。
    let trimmed = quoted_text.trim();
    if !trimmed.is_empty() {
        if let Some(byte) = block_text.find(trimmed) {
            let (s, e) = utf16_range_of(block_text, byte, byte + trimmed.len());
            return Some((trimmed.to_string(), Some((s, e)), false));
        }
    }
    // 规范化匹配:换行、软连字符、Unicode 兼容差异;命中后映射回原文切片。
    let hay = normalize_v1(block_text);
    let needle_src = if trimmed.is_empty() {
        quoted_text
    } else {
        trimmed
    };
    let needle = normalize_v1(needle_src);
    if let Some((start, end)) = hay.find_original_range(&needle) {
        let units: Vec<u16> = block_text.encode_utf16().collect();
        if end <= units.len() {
            let slice: String = String::from_utf16_lossy(&units[start..end]);
            if !slice.trim().is_empty() {
                return Some((slice, Some((start, end)), true));
            }
        }
    }
    None
}

/// Locator 结构可解析性:封闭联合 + 与文档格式匹配 + 基本范围合法。
fn locator_resolves(locator: &SourceLocator, format: DocumentFormat) -> bool {
    let kind_ok = match locator {
        SourceLocator::Pdf {
            page_number, rects, ..
        } => format == DocumentFormat::Pdf && *page_number >= 1 && !rects.is_empty(),
        SourceLocator::Markdown {
            start_offset,
            end_offset,
            ..
        } => format == DocumentFormat::Markdown && start_offset <= end_offset && *start_offset >= 0,
        SourceLocator::Docx {
            body_block_start,
            body_block_end,
            ..
        } => format == DocumentFormat::Docx && body_block_start <= body_block_end,
        SourceLocator::Composite { parts, .. } => {
            !parts.is_empty() && parts.iter().all(|p| locator_resolves(p, format))
        }
    };
    kind_ok
}

fn heading_path_of_locator(locator: &SourceLocator) -> Vec<String> {
    match locator {
        SourceLocator::Markdown { heading_path, .. } => heading_path.clone(),
        SourceLocator::Docx { heading_path, .. } => heading_path.clone(),
        _ => Vec::new(),
    }
}

/// 校验一条指向 ContentBlock 的文档事实引文(§4 流程图逐分支)。
fn validate_block_citation_tx(
    conn: &Connection,
    document_id: &str,
    version_id: &str,
    block_id: &str,
    quoted_text: &str,
    locator: Option<&SourceLocator>,
) -> Result<CitationValidation, AppError> {
    // 版本属于同一文档?
    let owner: Option<String> = conn
        .query_row(
            "SELECT document_id FROM document_version WHERE id=?1",
            [version_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    if owner.as_deref() != Some(document_id) {
        return Ok(CitationValidation {
            structural_status: StructuralStatus::VersionMismatch,
            block_id: Some(block_id.to_string()),
            saved_quote: Some(quoted_text.to_string()),
            quote_start_utf16: None,
            quote_end_utf16: None,
            normalized_match: false,
        });
    }
    // 块存在且属于该版本(代次不限:历史 Citation 指向原代次)。
    let Some(block) = load_block_tx(conn, block_id)? else {
        return Ok(CitationValidation {
            structural_status: StructuralStatus::TargetMissing,
            block_id: Some(block_id.to_string()),
            saved_quote: Some(quoted_text.to_string()),
            quote_start_utf16: None,
            quote_end_utf16: None,
            normalized_match: false,
        });
    };
    if block.document_version_id != version_id {
        return Ok(CitationValidation {
            structural_status: StructuralStatus::TargetMissing,
            block_id: Some(block_id.to_string()),
            saved_quote: Some(quoted_text.to_string()),
            quote_start_utf16: None,
            quote_end_utf16: None,
            normalized_match: false,
        });
    }
    // 原文精确/规范化包含引文?
    let Some((saved_quote, range, normalized)) = match_quote_in_text(&block.text, quoted_text)
    else {
        return Ok(CitationValidation {
            structural_status: StructuralStatus::QuoteMismatch,
            block_id: Some(block_id.to_string()),
            saved_quote: Some(quoted_text.to_string()),
            quote_start_utf16: None,
            quote_end_utf16: None,
            normalized_match: false,
        });
    };
    // Locator 可解析?
    let format =
        document_format_of(conn, document_id)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
    let block_locator: Option<SourceLocator> = serde_json::from_str(&block.locator_json).ok();
    let locator_ok = match locator {
        Some(loc) => locator_resolves(loc, format),
        None => block_locator
            .as_ref()
            .is_some_and(|l| locator_resolves(l, format)),
    };
    if !locator_ok {
        return Ok(CitationValidation {
            structural_status: StructuralStatus::LocatorInvalid,
            block_id: Some(block_id.to_string()),
            saved_quote: Some(saved_quote),
            quote_start_utf16: None,
            quote_end_utf16: None,
            normalized_match: normalized,
        });
    }
    Ok(CitationValidation {
        structural_status: StructuralStatus::Valid,
        block_id: Some(block_id.to_string()),
        saved_quote: Some(saved_quote),
        quote_start_utf16: range.map(|r| r.0),
        quote_end_utf16: range.map(|r| r.1),
        normalized_match: normalized,
    })
}

/// 属主删除时清理其 Citation(M05 §9):供 M06 等属主模块在
/// 自身删除事务内调用;同一事务完成归属校验与删除。
pub fn delete_citations_for_owner_tx(
    conn: &Connection,
    owner_type: CitationOwnerType,
    owner_id: &str,
) -> Result<i64, AppError> {
    // 先删反馈再删引用(引用删除后反馈悬空)。
    conn.execute(
        "DELETE FROM evidence_feedback WHERE owner_type=?1 AND owner_id=?2",
        params![owner_type.as_str(), owner_id],
    )
    .map_err(|e| db_app_error(&e))?;
    let n = conn
        .execute(
            "DELETE FROM citation WHERE owner_type=?1 AND owner_id=?2",
            params![owner_type.as_str(), owner_id],
        )
        .map_err(|e| db_app_error(&e))?;
    Ok(n as i64)
}

/// 锚点摘要(存在性 + 有效状态):M06 锚定笔记/标注的只读端口。
pub fn anchor_summary_tx(
    conn: &Connection,
    anchor_id: &str,
) -> Result<Option<AnchorSummary>, AppError> {
    let Some(row) = load_anchor_tx(conn, anchor_id)? else {
        return Ok(None);
    };
    let current = current_version_id_tx(conn, &row.document_id)?;
    let status = effective_anchor_status(row.status, &row.document_version_id, current.as_deref());
    Ok(Some(AnchorSummary {
        id: row.id,
        document_id: row.document_id,
        document_version_id: row.document_version_id,
        kind: row.kind,
        status,
    }))
}

/// 锚点完整视图(块引文/定位/overlay):M06 标注展示的只读端口。
pub fn anchor_view_by_id_tx(
    conn: &Connection,
    anchor_id: &str,
) -> Result<Option<AnchorView>, AppError> {
    match load_anchor_tx(conn, anchor_id)? {
        Some(row) => Ok(Some(anchor_view_tx(conn, &row)?)),
        None => Ok(None),
    }
}

// —— 服务 ——

pub struct EvidenceService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    parse: Arc<ParseIndexService>,
    owners: RwLock<OwnerValidators>,
}

impl EvidenceService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        parse: Arc<ParseIndexService>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            parse,
            owners: RwLock::new(OwnerValidators::default()),
        }
    }

    /// 注册属主校验器(M13 组装时接线;测试注入真实服务)。
    pub fn register_owner_validator(&self, owner_type: CitationOwnerType, check: OwnerCheckFn) {
        self.owners
            .write()
            .expect("evidence owner registry lock")
            .register(owner_type, check);
    }

    /// 已注册属主校验器的快照:需要在自己事务里写 Citation 的模块
    /// (M09 问答提交)必须使用同一份注册表,否则会以
    /// owner_validator_missing 拒绝(而不是默认放行)。
    pub fn owner_validators_snapshot(&self) -> Arc<OwnerValidators> {
        Arc::new(self.owner_validators())
    }

    fn owner_validators(&self) -> OwnerValidators {
        self.owners
            .read()
            .expect("evidence owner registry lock")
            .clone()
    }

    // —— Anchor 创建(《M05》§2)——

    /// `evidence_create_anchor`:RawSelection → Anchor(幂等)。
    pub fn create_anchor(
        &self,
        sel: &RawSelectionInput,
        idempotency_key: &str,
    ) -> Result<AnchorView, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        // 形状校验(纯函数,先于数据库)。
        validate_selection_shape(sel)?;
        // 幂等:同 key 直接返回已有锚点。
        {
            let key = idempotency_key.to_string();
            let existing = self.db.read(move |conn| {
                Ok(load_anchor_by_key_tx(conn, &key)?
                    .map(|row| anchor_view_tx(conn, &row))
                    .transpose())
            })??;
            if let Some(view) = existing {
                return Ok(view);
            }
        }

        // 每个选区块确保 on_demand ContentBlock(M04);块文本必须与选区一致,
        // 不伪造块引文。
        let mut resolved: Vec<(String, String)> = Vec::with_capacity(sel.blocks.len());
        let mut ordered: Vec<&RawSelectionBlockInput> = sel.blocks.iter().collect();
        ordered.sort_by_key(|b| b.block_order);
        for b in &ordered {
            let req = SelectionBlockRequest {
                document_id: sel.document_id.clone(),
                version_id: sel.document_version_id.clone(),
                text: b.text.clone(),
                locator: b.locator.clone(),
                heading_path: heading_path_of_locator(&b.locator),
            };
            let block = self.parse.ensure_selection_blocks(&req)?;
            if block.text != b.text {
                return Err(err(CoreErrorCode::InvalidRequest)
                    .with_detail("reason", "selection_text_mismatch".into()));
            }
            resolved.push((block.id, b.text.clone()));
        }

        let now = self.clock.now_ms();
        let anchor_id = self.ids.new_entity_id();
        let kind = sel
            .selection_kind
            .unwrap_or(SelectionKind::Text)
            .to_anchor_kind();
        let text_hash = sha256_str(&sel.text);
        let key = idempotency_key.to_string();
        let sel2 = sel.clone();
        self.db.write(move |conn| {
            // 事务内复核幂等(并发兜底)。
            if let Some(row) = load_anchor_by_key_tx(conn, &key)? {
                return anchor_view_tx(conn, &row);
            }
            conn.execute(
                "INSERT INTO anchor(id, document_id, document_version_id, kind, status, \
                 exact_text, prefix_text, suffix_text, snapshot_text, text_hash, \
                 primary_locator_json, idempotency_key, created_at) \
                 VALUES (?1,?2,?3,?4,'active',?5,'','',?6,?7,?8,?9,?10)",
                params![
                    anchor_id,
                    sel2.document_id,
                    sel2.document_version_id,
                    kind.as_str(),
                    sel2.text,
                    sel2.text,
                    text_hash,
                    serde_json::to_string(&sel2.primary_locator).unwrap_or_default(),
                    key,
                    now,
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            for (position, (block_id, quote)) in resolved.iter().enumerate() {
                conn.execute(
                    "INSERT INTO anchor_block(anchor_id, block_id, position, block_quote_text) \
                     VALUES (?1,?2,?3,?4)",
                    params![anchor_id, block_id, position as i64, quote],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            let row =
                load_anchor_tx(conn, &anchor_id)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            anchor_view_tx(conn, &row)
        })
    }

    pub fn get_anchor(&self, anchor_id: &str) -> Result<Option<AnchorView>, AppError> {
        let aid = anchor_id.to_string();
        self.db.read(move |conn| {
            Ok(load_anchor_tx(conn, &aid)?
                .map(|row| anchor_view_tx(conn, &row))
                .transpose())
        })?
    }

    /// 文档内锚点列表(标注/笔记面板用)。
    pub fn list_anchors(&self, document_id: &str) -> Result<Vec<AnchorView>, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {ANCHOR_COLS} FROM anchor WHERE document_id=?1 ORDER BY created_at, id"
                ))
                .map_err(|e| db_app_error(&e))?;
            let rows = stmt
                .query_map([&did], |r| AnchorRow::try_from(r))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            rows.iter().map(|row| anchor_view_tx(conn, row)).collect()
        })
    }

    // —— Citation 批量保存与校验(《M05》§3/§4/§9)——

    /// 批量保存(M09 在任务提交事务内调用 `save_citations_tx`;
    /// 此处为独立事务入口,测试与预览使用)。
    pub fn save_citations(
        &self,
        req: &SaveCitationsRequest,
    ) -> Result<Vec<CitationView>, AppError> {
        let validators = self.owner_validators();
        let now = self.clock.now_ms();
        let ids: Vec<String> = (0..req.citations.len())
            .map(|_| self.ids.new_entity_id())
            .collect();
        let req2 = req.clone();
        self.db
            .write(move |conn| save_citations_tx(conn, &req2, &validators, &ids, now))
    }

    /// 解析单条 Citation(透镜与回跳共享):不写库。
    pub fn inspect_citation(
        &self,
        citation_id: &str,
    ) -> Result<Option<(CitationRow, CitationValidation)>, AppError> {
        let cid = citation_id.to_string();
        self.db.read(move |conn| {
            let Some(row) = load_citation_tx(conn, &cid)? else {
                return Ok(None);
            };
            let validation = inspect_citation_tx(conn, &row)?;
            Ok(Some((row, validation)))
        })
    }

    // —— 回跳(《M05》§6)——

    /// `evidence_resolve_citation`:先检查结构,再给出回跳定位或快照;
    /// 失败显示快照与原因,不打开错误页。
    /// 2B:按稳定块 ID 返回可回跳定位(卡片/题目引用;不伪造位置)。
    pub fn resolve_block_locator(
        &self,
        block_id: &str,
    ) -> Result<Option<crate::shared::locator::SourceLocator>, AppError> {
        let block_id = block_id.to_string();
        self.db.read(move |conn| {
            let Some(block) = load_block_tx(conn, &block_id)? else {
                return Ok(None);
            };
            Ok(serde_json::from_str(&block.locator_json).ok())
        })
    }

    pub fn resolve_citation(&self, citation_id: &str) -> Result<EvidenceResolution, AppError> {
        let cid = citation_id.to_string();
        self.db.read(move |conn| {
            let row = load_citation_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let validation = inspect_citation_tx(conn, &row)?;
            let current = current_version_id_tx(conn, &row.document_id)?;
            let mut res = EvidenceResolution {
                citation_id: row.id.clone(),
                structural_status: validation.structural_status,
                mode: ResolutionMode::Snapshot,
                locator: None,
                page_number: None,
                block_id: validation.block_id.clone(),
                block_key: None,
                heading_path: None,
                quote_start_utf16: None,
                quote_end_utf16: None,
                quote_text: row.quoted_text.clone(),
                snapshot_text: Some(row.source_snapshot.clone()),
                document_version_id: row.document_version_id.clone(),
                reason: None,
            };
            if validation.structural_status != StructuralStatus::Valid {
                res.reason = Some(format!(
                    "structural_{}",
                    validation.structural_status.as_str()
                ));
                return Ok(res);
            }
            // 结构有效:填块定位信息。
            if let Some(block_id) = &validation.block_id {
                if let Some(block) = load_block_tx(conn, block_id)? {
                    res.block_key = Some(block.block_key.clone());
                    res.heading_path = serde_json::from_str(&block.heading_path_json).ok();
                    let loc: Option<SourceLocator> = serde_json::from_str(&block.locator_json).ok();
                    if let Some(loc) = loc {
                        res.page_number = loc.pdf_page();
                        res.locator = Some(loc);
                    }
                }
            }
            res.quote_start_utf16 = validation.quote_start_utf16.map(|v| v as i64);
            res.quote_end_utf16 = validation.quote_end_utf16.map(|v| v as i64);
            // 非原文来源(笔记/AI 草稿):不作为原文链接回跳(《M05》§5)。
            if row.source_type.source_tier() != SourceTier::Original || res.locator.is_none() {
                res.reason = Some("non_document_source".to_string());
                return Ok(res);
            }
            // 旧引用默认不自动跳到新版本:版本被取代 ⇒ 快照模式 + 原因。
            if current.as_deref() != Some(row.document_version_id.as_str()) {
                res.reason = Some("superseded_version".to_string());
                if let Some(anchor_id) = &row.anchor_id {
                    if let Some(anchor) = load_anchor_tx(conn, anchor_id)? {
                        res.snapshot_text = Some(anchor.snapshot_text);
                    }
                }
                return Ok(res);
            }
            res.mode = ResolutionMode::Navigate;
            Ok(res)
        })
    }

    // —— 证据透镜(《M05》§5)——

    /// `evidence_get_lens`:结论引文按原文顺序 + 来源层级 + 缺失/冲突证据。
    pub fn get_lens(
        &self,
        owner_type: CitationOwnerType,
        owner_id: &str,
    ) -> Result<EvidenceLensView, AppError> {
        let ot = owner_type.as_str().to_string();
        let oid = owner_id.to_string();
        self.db.read(move |conn| {
            let mut stmt = conn
                .prepare(&format!(
                    "SELECT {CITATION_COLS}, \
                        (SELECT cb.ordinal FROM content_block cb \
                         WHERE cb.id = citation.content_block_id) AS block_order \
                     FROM citation WHERE owner_type=?1 AND owner_id=?2 \
                     ORDER BY (block_order IS NULL), block_order, created_at, id"
                ))
                .map_err(|e| db_app_error(&e))?;
            let rows: Vec<(CitationRow, Option<i64>)> = stmt
                .query_map(params![ot, oid], |r| {
                    Ok((CitationRow::try_from(r)?, r.get("block_order")?))
                })
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);

            let mut citations = Vec::new();
            let mut missing = Vec::new();
            let mut conflicts = Vec::new();
            let mut order = 0i64;
            for (row, block_order) in &rows {
                let page_number = row
                    .content_block_id
                    .as_ref()
                    .and_then(|bid| load_block_tx(conn, bid).ok().flatten())
                    .and_then(|b| {
                        serde_json::from_str::<SourceLocator>(&b.locator_json)
                            .ok()
                            .and_then(|l| l.pdf_page())
                    });
                let heading_path = row
                    .content_block_id
                    .as_ref()
                    .and_then(|bid| load_block_tx(conn, bid).ok().flatten())
                    .and_then(|b| serde_json::from_str::<Vec<String>>(&b.heading_path_json).ok());
                let anchor_status = row
                    .anchor_id
                    .as_ref()
                    .and_then(|aid| anchor_summary_tx(conn, aid).ok().flatten())
                    .map(|a| a.status);
                order += 1;
                citations.push(LensCitationView {
                    citation_id: row.id.clone(),
                    claim_key: row.claim_key.clone(),
                    claim_kind: row.claim_kind,
                    source_type: row.source_type,
                    source_tier: row.source_type.source_tier(),
                    quoted_text: row.quoted_text.clone(),
                    structural_status: row.structural_status,
                    support_status: row.support_status,
                    support_source: row.support_source,
                    document_version_id: row.document_version_id.clone(),
                    order: block_order.unwrap_or(order),
                    page_number,
                    heading_path,
                    anchor_id: row.anchor_id.clone(),
                    anchor_status,
                });
                if row.structural_status != StructuralStatus::Valid {
                    missing.push(LensIssueView {
                        citation_id: row.id.clone(),
                        claim_key: row.claim_key.clone(),
                        issue_kind: "structural_invalid".to_string(),
                        description: format!(
                            "引文结构校验未通过({}),原文位置不可回跳",
                            row.structural_status.as_str()
                        ),
                    });
                } else if row.support_status == SupportStatus::Insufficient {
                    missing.push(LensIssueView {
                        citation_id: row.id.clone(),
                        claim_key: row.claim_key.clone(),
                        issue_kind: "missing_evidence".to_string(),
                        description: "支持证据不足".to_string(),
                    });
                }
                if row.support_status == SupportStatus::Conflict {
                    conflicts.push(LensIssueView {
                        citation_id: row.id.clone(),
                        claim_key: row.claim_key.clone(),
                        issue_kind: "conflict".to_string(),
                        description: "证据与结论存在冲突".to_string(),
                    });
                }
            }

            // 冲突类反馈也计入冲突区(反馈不改写状态,只提示)。
            let ids: Vec<String> = rows.iter().map(|(r, _)| r.id.clone()).collect();
            let mut feedback_count = 0i64;
            let mut feedback_conflicts = Vec::new();
            for cid in &ids {
                let (count, conflict): (i64, i64) = conn
                    .query_row(
                        "SELECT COUNT(*), SUM(CASE WHEN feedback_type='conflict_with_source' \
                         THEN 1 ELSE 0 END) FROM evidence_feedback WHERE citation_id=?1",
                        [cid],
                        |r| Ok((r.get(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))),
                    )
                    .map_err(|e| db_app_error(&e))?;
                feedback_count += count;
                if conflict > 0 {
                    if let Some((row, _)) = rows.iter().find(|(r, _)| &r.id == cid) {
                        feedback_conflicts.push(LensIssueView {
                            citation_id: row.id.clone(),
                            claim_key: row.claim_key.clone(),
                            issue_kind: "feedback_conflict".to_string(),
                            description: "用户反馈该证据与原文冲突".to_string(),
                        });
                    }
                }
            }
            conflicts.extend(feedback_conflicts);

            let document_id = rows
                .first()
                .map(|(r, _)| r.document_id.clone())
                .unwrap_or_default();
            Ok(EvidenceLensView {
                owner_type,
                owner_id: oid,
                document_id,
                citations,
                missing_evidence: missing,
                conflicts,
                feedback_count,
            })
        })
    }

    // —— 反馈与支持确认(《M05》§4/§5)——

    /// `evidence_feedback`:append-only;保留原结果,不改写历史状态。
    pub fn submit_feedback(
        &self,
        citation_id: &str,
        feedback_type: EvidenceFeedbackType,
        comment: &str,
        idempotency_key: &str,
    ) -> Result<FeedbackResult, AppError> {
        if idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let cid = citation_id.to_string();
        let ft = feedback_type.as_str().to_string();
        let cmt = comment.to_string();
        let key = idempotency_key.to_string();
        let now = self.clock.now_ms();
        self.db.write(move |conn| {
            let row = load_citation_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            // 幂等:同 key 返回已有反馈。
            let existing: Option<(String, i64)> = conn
                .query_row(
                    "SELECT id, created_at FROM evidence_feedback WHERE idempotency_key=?1",
                    [&key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            if let Some((fid, created)) = existing {
                return Ok(FeedbackResult {
                    feedback_id: fid,
                    citation_id: cid.clone(),
                    feedback_type,
                    total_count: count_feedback_tx(conn, &cid)?,
                    created_at: created,
                });
            }
            let fid = crate::shared::new_trace_id(); // 反馈行 ID 不参与业务排序
            conn.execute(
                "INSERT INTO evidence_feedback(id, citation_id, owner_type, owner_id, \
                 feedback_type, comment, idempotency_key, created_at) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    fid,
                    cid,
                    row.owner_type.as_str(),
                    row.owner_id,
                    ft,
                    cmt,
                    key,
                    now
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(FeedbackResult {
                feedback_id: fid,
                citation_id: cid.clone(),
                feedback_type,
                total_count: count_feedback_tx(conn, &cid)?,
                created_at: now,
            })
        })
    }

    /// `evidence_confirm_support`:用户确认支持;只有结构有效的引文可确认。
    /// 写入 supportSource=user;不悄悄核验历史结果之外的东西。
    pub fn confirm_support(
        &self,
        citation_id: &str,
        _idempotency_key: &str,
    ) -> Result<CitationView, AppError> {
        let cid = citation_id.to_string();
        self.db.write(move |conn| {
            let row = load_citation_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.structural_status != StructuralStatus::Valid {
                return Err(err(row.structural_status.error_code())
                    .with_detail("citationId", cid.as_str().into()));
            }
            // 幂等:已由用户确认 ⇒ 原样返回。
            if row.support_status == SupportStatus::Supported
                && row.support_source == SupportSource::User
            {
                return Ok(row.to_view());
            }
            conn.execute(
                "UPDATE citation SET support_status='supported', support_source='user' \
                 WHERE id=?1",
                params![cid],
            )
            .map_err(|e| db_app_error(&e))?;
            load_citation_tx(conn, &cid)?
                .map(|r| r.to_view())
                .ok_or_else(|| err(CoreErrorCode::Internal))
        })
    }

    // —— 重定位(《M05》§7)——

    /// `evidence_propose_relocation`:生成候选(proposed),不自动绑定。
    pub fn propose_relocation(
        &self,
        anchor_id: &str,
        target_version_id: &str,
    ) -> Result<Vec<RelocationCandidate>, AppError> {
        let aid = anchor_id.to_string();
        let tid = target_version_id.to_string();

        // 读取旧锚点与目标代次块(只读)。
        let tid_for_read = tid.clone();
        let (anchor, anchor_blocks, target_blocks) = self.db.read(move |conn| {
            let anchor = load_anchor_tx(conn, &aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let blocks = load_anchor_blocks_tx(conn, &anchor.id)?;
            // 目标版本必须属于同一文档且指纹就绪。
            let _format = validate_version_tx(conn, &anchor.document_id, &tid_for_read)?;
            let target_blocks = load_current_generation_blocks_tx(conn, &tid_for_read)?;
            Ok((anchor, blocks, target_blocks))
        })?;

        // 候选生成:每块最优匹配;全部命中且目标序单调才形成锚点级候选。
        let mut candidates =
            build_relocation_candidates(&anchor, &anchor_blocks, &target_blocks, &tid);
        if candidates.len() > MAX_RELOCATION_CANDIDATES {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "relocation_candidate_overflow".into()));
        }

        let now = self.clock.now_ms();
        for c in &mut candidates {
            c.created_at = now;
        }
        let aid2 = anchor_id.to_string();
        let tid2 = target_version_id.to_string();
        let cand2 = candidates.clone();
        self.db.write(move |conn| {
            // 替换旧的 proposed(保留 confirmed/rejected 历史)。
            conn.execute(
                "DELETE FROM anchor_relocation \
                 WHERE anchor_id=?1 AND target_version_id=?2 AND status='proposed'",
                params![aid2, tid2],
            )
            .map_err(|e| db_app_error(&e))?;
            for c in &cand2 {
                conn.execute(
                    "INSERT INTO anchor_relocation(id, anchor_id, target_version_id, \
                     candidate_json, score, status, created_at) \
                     VALUES (?1,?2,?3,?4,?5,'proposed',?6)",
                    params![
                        c.id,
                        aid2,
                        tid2,
                        serde_json::to_string(&RelocationCandidateJson::from_candidate(c))
                            .unwrap_or_default(),
                        c.score,
                        c.created_at
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            Ok(cand2)
        })
    }

    /// `evidence_confirm_relocation`:确认候选 ⇒ 新建 relocated Anchor
    /// (保留原快照);旧 Anchor 历史保持。
    pub fn confirm_relocation(
        &self,
        anchor_id: &str,
        candidate_id: &str,
        _idempotency_key: &str,
    ) -> Result<AnchorView, AppError> {
        let aid = anchor_id.to_string();
        let cid = candidate_id.to_string();
        let key = format!("relocation:{cid}");
        // 幂等身份由候选 ID 决定:同一候选只产生一个新锚点。
        let now = self.clock.now_ms();
        let new_anchor_id = self.ids.new_entity_id();
        self.db.write(move |conn| {
            // 幂等:同候选已确认 ⇒ 返回已建锚点。
            if let Some(row) = load_anchor_by_key_tx(conn, &key)? {
                return anchor_view_tx(conn, &row);
            }
            let anchor = load_anchor_tx(conn, &aid)?.ok_or_else(|| err(CoreErrorCode::NotFound))?;
            let cand: Option<(String, String)> = conn
                .query_row(
                    "SELECT status, target_version_id FROM anchor_relocation \
                     WHERE id=?1 AND anchor_id=?2",
                    params![cid, aid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            let (status, target_version) =
                cand.ok_or_else(|| err(CoreErrorCode::AnchorRelocationNotConfirmed))?;
            if status != "proposed" {
                return Err(err(CoreErrorCode::AnchorRelocationNotConfirmed));
            }
            let cand_view = load_relocation_tx(conn, &cid)?
                .ok_or_else(|| err(CoreErrorCode::AnchorRelocationNotConfirmed))?;
            let mappings = cand_view.mappings.clone();
            if mappings.is_empty() {
                return Err(err(CoreErrorCode::AnchorRelocationNotConfirmed));
            }
            // 新锚点:绑定目标版本;exact_text 为新版本匹配文本,快照保留原文。
            let matched_text = mappings
                .iter()
                .map(|m| m.matched_text.clone())
                .collect::<Vec<_>>()
                .join("\n");
            let primary_locator = load_block_tx(conn, &mappings[0].to_block_id)?
                .and_then(|b| serde_json::from_str::<SourceLocator>(&b.locator_json).ok())
                .unwrap_or_else(|| {
                    serde_json::from_str(&anchor.primary_locator_json).unwrap_or(
                        SourceLocator::Composite {
                            parts: vec![],
                            locator_version: 1,
                        },
                    )
                });
            conn.execute(
                "INSERT INTO anchor(id, document_id, document_version_id, kind, status, \
                 exact_text, prefix_text, suffix_text, snapshot_text, text_hash, \
                 primary_locator_json, idempotency_key, created_at) \
                 VALUES (?1,?2,?3,?4,'relocated',?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    new_anchor_id,
                    anchor.document_id,
                    target_version,
                    anchor.kind.as_str(),
                    matched_text,
                    anchor.prefix_text,
                    anchor.suffix_text,
                    anchor.snapshot_text,
                    sha256_str(&matched_text),
                    serde_json::to_string(&primary_locator).unwrap_or_default(),
                    key,
                    now,
                ],
            )
            .map_err(|e| db_app_error(&e))?;
            for (position, m) in mappings.iter().enumerate() {
                conn.execute(
                    "INSERT INTO anchor_block(anchor_id, block_id, position, block_quote_text) \
                     VALUES (?1,?2,?3,?4)",
                    params![
                        new_anchor_id,
                        m.to_block_id,
                        position as i64,
                        m.matched_text
                    ],
                )
                .map_err(|e| db_app_error(&e))?;
            }
            conn.execute(
                "UPDATE anchor_relocation SET status='confirmed', confirmed_at=?2 WHERE id=?1",
                params![cid, now],
            )
            .map_err(|e| db_app_error(&e))?;
            let row = load_anchor_tx(conn, &new_anchor_id)?
                .ok_or_else(|| err(CoreErrorCode::Internal))?;
            anchor_view_tx(conn, &row)
        })
    }

    /// `evidence_reject_relocation`:用户选择"保持孤立"。
    pub fn reject_relocation(
        &self,
        anchor_id: &str,
        candidate_id: &str,
    ) -> Result<RelocationCandidate, AppError> {
        let aid = anchor_id.to_string();
        let cid = candidate_id.to_string();
        self.db.write(move |conn| {
            let row = load_relocation_row_tx(conn, &cid)?
                .filter(|c| c.anchor_id == aid)
                .ok_or_else(|| err(CoreErrorCode::NotFound))?;
            if row.status != RelocationStatus::Proposed {
                return Ok(row_to_candidate_view(&row));
            }
            conn.execute(
                "UPDATE anchor_relocation SET status='rejected' WHERE id=?1",
                params![cid],
            )
            .map_err(|e| db_app_error(&e))?;
            let updated =
                load_relocation_row_tx(conn, &cid)?.ok_or_else(|| err(CoreErrorCode::Internal))?;
            Ok(row_to_candidate_view(&updated))
        })
    }

    // —— 证据保留裁决(《M05》§8)——

    /// 返回可清理/受保护块与原因。M08 清解析缓存前必须调用;
    /// M05 不执行删除。保护条件:被 AnchorBlock/Citation 引用、
    /// 属于仍保留快照(M07 表落地后接入)、或当前代次(可问答能力)。
    pub fn evaluate_evidence_retention(
        &self,
        document_id: &str,
        requested_categories: &[RetentionCategory],
    ) -> Result<RetentionRuling, AppError> {
        let did = document_id.to_string();
        let cats: Vec<String> = requested_categories
            .iter()
            .map(|c| c.as_str().to_string())
            .collect();
        self.db.read(move |conn| {
            // 文档必须存在(裁决以文档为界)。
            document_format_of(conn, &did)?.ok_or_else(|| err(CoreErrorCode::DocNotFound))?;
            // 当前版本与当前代次。
            let current_version = current_version_id_tx(conn, &did)?;
            let current_gen: Option<String> = current_version.as_ref().and_then(|vid| {
                conn.query_row(
                    "SELECT id FROM parse_generation WHERE document_version_id=?1 \
                         AND is_current=1 LIMIT 1",
                    [vid],
                    |r| r.get(0),
                )
                .optional()
                .ok()
                .flatten()
            });

            // 引用保护集(M05 自己的表)。
            let mut protected: HashMap<String, &'static str> = HashMap::new();
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT ab.block_id FROM anchor_block ab \
                     JOIN anchor a ON a.id = ab.anchor_id WHERE a.document_id=?1",
                )
                .map_err(|e| db_app_error(&e))?;
            let anchored: Vec<String> = stmt
                .query_map([&did], |r| r.get(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            for b in &anchored {
                protected.insert(b.clone(), "anchor_reference");
            }
            let mut stmt = conn
                .prepare(
                    "SELECT DISTINCT content_block_id FROM citation \
                     WHERE document_id=?1 AND content_block_id IS NOT NULL",
                )
                .map_err(|e| db_app_error(&e))?;
            let cited: Vec<String> = stmt
                .query_map([&did], |r| r.get(0))
                .map_err(|e| db_app_error(&e))?
                .collect::<Result<_, _>>()
                .map_err(|e| db_app_error(&e))?;
            drop(stmt);
            for b in &cited {
                protected.entry(b.clone()).or_insert("citation_reference");
            }
            // 当前代次解析块:当前可问答能力的必要组成。
            if let Some(gen) = &current_gen {
                let mut stmt = conn
                    .prepare(
                        "SELECT id FROM content_block WHERE parse_generation_id=?1 \
                         AND origin IN ('parser','ocr')",
                    )
                    .map_err(|e| db_app_error(&e))?;
                let current_blocks: Vec<String> = stmt
                    .query_map([gen], |r| r.get(0))
                    .map_err(|e| db_app_error(&e))?
                    .collect::<Result<_, _>>()
                    .map_err(|e| db_app_error(&e))?;
                drop(stmt);
                for b in current_blocks {
                    protected.entry(b).or_insert("current_generation");
                }
            }

            // 各类别裁决。
            let mut categories = Vec::new();
            let mut protected_views: Vec<ProtectedBlockView> = protected
                .iter()
                .map(|(id, reason)| ProtectedBlockView {
                    block_id: id.clone(),
                    reason: reason.to_string(),
                })
                .collect();
            protected_views.sort_by(|a, b| a.block_id.cmp(&b.block_id));

            for cat in &cats {
                let category = RetentionCategory::parse_db(cat)
                    .ok_or_else(|| err(CoreErrorCode::InvalidRequest))?;
                match category {
                    RetentionCategory::FtsIndex | RetentionCategory::VectorIndex => {
                        // 可重建缓存:清除不影响引用,能力临时降级由 M08 提示。
                        let count: i64 = match category {
                            RetentionCategory::FtsIndex => conn
                                .query_row(
                                    "SELECT COUNT(*) FROM content_block_fts f \
                                     JOIN parse_generation g ON g.id = f.parse_generation_id \
                                     JOIN document_version v ON v.id = g.document_version_id \
                                     WHERE v.document_id=?1",
                                    [&did],
                                    |r| r.get(0),
                                )
                                .map_err(|e| db_app_error(&e))?,
                            _ => conn
                                .query_row(
                                    "SELECT COUNT(*) FROM vector_embedding ve \
                                     JOIN embedding_generation eg ON eg.id = ve.generation_id \
                                     JOIN parse_generation g ON g.id = eg.parse_generation_id \
                                     JOIN document_version v ON v.id = g.document_version_id \
                                     WHERE v.document_id=?1",
                                    [&did],
                                    |r| r.get(0),
                                )
                                .map_err(|e| db_app_error(&e))?,
                        };
                        categories.push(RetentionCategoryRuling {
                            category,
                            protected_count: 0,
                            cleanable_count: count,
                        });
                    }
                    RetentionCategory::ParseCache => {
                        // 可清理 = 本文档解析块中非当前代次且未被引用 + 未被引用的 on_demand。
                        let mut stmt = conn
                            .prepare(
                                "SELECT c.id, c.parse_generation_id FROM content_block c \
                                 JOIN document_version v ON v.id = c.document_version_id \
                                 WHERE v.document_id=?1",
                            )
                            .map_err(|e| db_app_error(&e))?;
                        let blocks: Vec<(String, Option<String>)> = stmt
                            .query_map([&did], |r| Ok((r.get(0)?, r.get(1)?)))
                            .map_err(|e| db_app_error(&e))?
                            .collect::<Result<_, _>>()
                            .map_err(|e| db_app_error(&e))?;
                        drop(stmt);
                        let mut cleanable = 0i64;
                        let mut prot = 0i64;
                        for (bid, gen) in &blocks {
                            if protected.contains_key(bid) {
                                prot += 1;
                                continue;
                            }
                            match gen {
                                Some(g) if Some(g) == current_gen.as_ref() => prot += 1,
                                _ => cleanable += 1,
                            }
                        }
                        categories.push(RetentionCategoryRuling {
                            category,
                            protected_count: prot,
                            cleanable_count: cleanable,
                        });
                    }
                }
            }
            Ok(RetentionRuling {
                document_id: did,
                categories,
                protected_blocks: protected_views,
            })
        })
    }
}

// —— 自由函数:事务内核心(供 M09 提交事务与 M06 属主删除复用)——

fn load_citation_tx(conn: &Connection, citation_id: &str) -> Result<Option<CitationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {CITATION_COLS} FROM citation WHERE id=?1"),
        [citation_id],
        |r| CitationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn count_feedback_tx(conn: &Connection, citation_id: &str) -> Result<i64, AppError> {
    conn.query_row(
        "SELECT COUNT(*) FROM evidence_feedback WHERE citation_id=?1",
        [citation_id],
        |r| r.get(0),
    )
    .map_err(|e| db_app_error(&e))
}

/// 对已保存 Citation 重跑结构校验(块不可变,通常与保存时一致;
/// 解析缓存被清理后可能退化为 target_missing)。
fn inspect_citation_tx(
    conn: &Connection,
    row: &CitationRow,
) -> Result<CitationValidation, AppError> {
    match (&row.content_block_id, &row.anchor_id) {
        (Some(block_id), _) => validate_block_citation_tx(
            conn,
            &row.document_id,
            &row.document_version_id,
            block_id,
            &row.quoted_text,
            None, // 保存时已校验定位;回跳使用块定位
        ),
        (None, Some(anchor_id)) => {
            // 锚点来源:在锚点各块中逐一匹配,取首个结构有效的块。
            let anchor = load_anchor_tx(conn, anchor_id)?
                .ok_or_else(|| err(CoreErrorCode::CitationTargetMissing))?;
            for ab in load_anchor_blocks_tx(conn, anchor_id)? {
                if match_quote_in_text(&ab.block_quote_text, &row.quoted_text).is_some() {
                    return validate_block_citation_tx(
                        conn,
                        &row.document_id,
                        &anchor.document_version_id,
                        &ab.block_id,
                        &row.quoted_text,
                        None,
                    );
                }
            }
            Ok(CitationValidation {
                structural_status: StructuralStatus::QuoteMismatch,
                block_id: None,
                saved_quote: Some(row.quoted_text.clone()),
                quote_start_utf16: None,
                quote_end_utf16: None,
                normalized_match: false,
            })
        }
        (None, None) => {
            // 非块来源(note/message 等):结构状态按来源可解析性。
            let resolvable = match row.source_type {
                CitationSourceType::Note => row
                    .source_id
                    .as_deref()
                    .is_some_and(|sid| note_source_exists_tx(conn, sid, &row.document_id)),
                CitationSourceType::Message
                | CitationSourceType::ArtifactVersion
                | CitationSourceType::AiDraft
                | CitationSourceType::UserView
                | CitationSourceType::Selection => row.source_id.is_some(),
                CitationSourceType::ContentBlock | CitationSourceType::Anchor => false,
            };
            Ok(CitationValidation {
                structural_status: if resolvable {
                    StructuralStatus::Valid
                } else {
                    StructuralStatus::TargetMissing
                },
                block_id: None,
                saved_quote: Some(row.quoted_text.clone()),
                quote_start_utf16: None,
                quote_end_utf16: None,
                normalized_match: false,
            })
        }
    }
}

/// note 来源存在性(M06 表;由 M06 注册的属主校验器同一实现承载)。
fn note_source_exists_tx(conn: &Connection, note_id: &str, document_id: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM note WHERE id=?1 AND document_id=?2",
        params![note_id, document_id],
        |_| Ok(()),
    )
    .optional()
    .is_ok_and(|x| x.is_some())
}

/// 批量保存核心:校验 + 插入,同一事务;幂等键按批次序号派生。
/// 供 M09 在任务提交事务内调用(《01》§9"提交回答"行)。
pub fn save_citations_tx(
    conn: &Connection,
    req: &SaveCitationsRequest,
    validators: &OwnerValidators,
    new_ids: &[String],
    now_ms: i64,
) -> Result<Vec<CitationView>, AppError> {
    if req.citations.len() > MAX_CITATIONS_PER_BATCH {
        return Err(err(CoreErrorCode::InvalidRequest)
            .with_detail("reason", "citation_batch_limit".into())
            .with_detail("max", (MAX_CITATIONS_PER_BATCH as i64).into())
            .with_detail("got", (req.citations.len() as i64).into()));
    }
    if new_ids.len() != req.citations.len() {
        return Err(err(CoreErrorCode::Internal));
    }
    // 属主校验:注册的校验器在同一事务连接上执行(能看到同事务未提交行)。
    let check = validators.get(&req.owner_type).ok_or_else(|| {
        err(CoreErrorCode::InvalidRequest).with_detail("reason", "owner_validator_missing".into())
    })?;
    if !check(conn, &req.owner_id, &req.document_id)? {
        return Err(
            err(CoreErrorCode::InvalidRequest).with_detail("reason", "owner_not_found".into())
        );
    }
    // 请求级版本归属:不属于同一文档 ⇒ 每条记 version_mismatch(仍保存,透镜可见)。
    let version_owner: Option<String> = conn
        .query_row(
            "SELECT document_id FROM document_version WHERE id=?1",
            [&req.document_version_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| db_app_error(&e))?;
    let version_ok = version_owner.as_deref() == Some(req.document_id.as_str());

    let mut out = Vec::with_capacity(req.citations.len());
    for (idx, input) in req.citations.iter().enumerate() {
        let key = format!("{}#{idx}", req.idempotency_key);
        // 幂等:同键已存在且属主一致 ⇒ 复用。
        if let Some(existing) = load_citation_by_key_tx(conn, &key)? {
            if existing.owner_type == req.owner_type && existing.owner_id == req.owner_id {
                out.push(existing.to_view());
                continue;
            }
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "citation_idempotency_conflict".into()));
        }

        // 文档事实规则:AI 草稿/笔记不得作为唯一原文证据(《M05》§3、A9/DATA-08)。
        if input.claim_kind == ClaimKind::DocumentFact
            && !input.source_type.is_document_fact_source()
        {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "document_fact_source_invalid".into())
                .with_detail("index", (idx as i64).into()));
        }
        // 支持来源规则:创建时不得写 user(用户确认走独立命令)。
        if input.support_source == Some(SupportSource::User) {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "support_source_user_forbidden".into())
                .with_detail("index", (idx as i64).into()));
        }

        // 按来源类型解析目标块并校验。
        let (block_id, anchor_id, validation, source_id) = match input.source_type {
            CitationSourceType::ContentBlock => {
                let bid = input
                    .content_block_id
                    .clone()
                    .or_else(|| input.source_id.clone())
                    .ok_or_else(|| {
                        err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "content_block_required".into())
                    })?;
                let v = if version_ok {
                    validate_block_citation_tx(
                        conn,
                        &req.document_id,
                        &req.document_version_id,
                        &bid,
                        &input.quoted_text,
                        input.locator.as_ref(),
                    )?
                } else {
                    CitationValidation {
                        structural_status: StructuralStatus::VersionMismatch,
                        block_id: Some(bid.clone()),
                        saved_quote: Some(input.quoted_text.clone()),
                        quote_start_utf16: None,
                        quote_end_utf16: None,
                        normalized_match: false,
                    }
                };
                (
                    Some(bid),
                    None,
                    v,
                    input.source_id.clone().or(input.content_block_id.clone()),
                )
            }
            CitationSourceType::Anchor => {
                let aid = input
                    .anchor_id
                    .clone()
                    .or_else(|| input.source_id.clone())
                    .ok_or_else(|| {
                        err(CoreErrorCode::InvalidRequest)
                            .with_detail("reason", "anchor_required".into())
                    })?;
                let anchor = load_anchor_tx(conn, &aid)?.ok_or_else(|| {
                    err(CoreErrorCode::CitationTargetMissing)
                        .with_detail("reason", "anchor_not_found".into())
                })?;
                if anchor.document_id != req.document_id {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "anchor_document_mismatch".into()));
                }
                // 引用与锚点必须同版本(块目标都在锚点版本上)。
                if anchor.document_version_id != req.document_version_id {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "anchor_version_mismatch".into()));
                }
                let mut chosen: Option<(String, CitationValidation)> = None;
                for ab in load_anchor_blocks_tx(conn, &aid)? {
                    let v = validate_block_citation_tx(
                        conn,
                        &req.document_id,
                        &anchor.document_version_id,
                        &ab.block_id,
                        &input.quoted_text,
                        input.locator.as_ref(),
                    )?;
                    if v.structural_status == StructuralStatus::Valid {
                        chosen = Some((ab.block_id.clone(), v));
                        break;
                    }
                }
                let (bid, v) = chosen.unwrap_or((
                    anchor.id.clone(), // 占位;状态将是 mismatch/missing
                    CitationValidation {
                        structural_status: StructuralStatus::QuoteMismatch,
                        block_id: None,
                        saved_quote: Some(input.quoted_text.clone()),
                        quote_start_utf16: None,
                        quote_end_utf16: None,
                        normalized_match: false,
                    },
                ));
                let bid = if v.structural_status == StructuralStatus::Valid {
                    Some(bid)
                } else {
                    v.block_id.clone()
                };
                (
                    bid,
                    Some(aid),
                    v,
                    input.source_id.clone().or(input.anchor_id.clone()),
                )
            }
            CitationSourceType::Note => {
                let sid = input.source_id.as_deref().ok_or_else(|| {
                    err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "source_id_required".into())
                })?;
                if !note_source_exists_tx(conn, sid, &req.document_id) {
                    return Err(err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "note_source_not_found".into()));
                }
                (
                    None,
                    None,
                    CitationValidation {
                        structural_status: StructuralStatus::Valid,
                        block_id: None,
                        saved_quote: Some(input.quoted_text.clone()),
                        quote_start_utf16: None,
                        quote_end_utf16: None,
                        normalized_match: false,
                    },
                    input.source_id.clone(),
                )
            }
            CitationSourceType::Message
            | CitationSourceType::ArtifactVersion
            | CitationSourceType::AiDraft
            | CitationSourceType::UserView
            | CitationSourceType::Selection => {
                let sid = input.source_id.as_deref().ok_or_else(|| {
                    err(CoreErrorCode::InvalidRequest)
                        .with_detail("reason", "source_id_required".into())
                })?;
                (
                    None,
                    None,
                    CitationValidation {
                        structural_status: StructuralStatus::Valid,
                        block_id: None,
                        saved_quote: Some(input.quoted_text.clone()),
                        quote_start_utf16: None,
                        quote_end_utf16: None,
                        normalized_match: false,
                    },
                    Some(sid.to_string()),
                )
            }
        };

        let saved_quote = validation
            .saved_quote
            .unwrap_or_else(|| input.quoted_text.clone());
        let snapshot = serde_json::json!({
            "quotedText": saved_quote,
            "normalizationVersion": crate::parse_index::NORMALIZATION_VERSION,
            "normalizedMatch": validation.normalized_match,
        })
        .to_string();
        let support_status = input.support_status.unwrap_or(SupportStatus::Unverified);
        let support_source = input.support_source.unwrap_or(SupportSource::None);
        let structural = if !version_ok {
            StructuralStatus::VersionMismatch
        } else {
            validation.structural_status
        };
        let block_col = if structural == StructuralStatus::Valid {
            block_id.clone()
        } else {
            validation.block_id.clone().or(block_id)
        };
        conn.execute(
            "INSERT INTO citation(id, document_id, document_version_id, owner_type, owner_id, \
             claim_key, claim_kind, source_type, source_id, content_block_id, anchor_id, \
             quoted_text, source_snapshot, structural_status, support_status, support_source, \
             idempotency_key, created_at) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                new_ids[idx],
                req.document_id,
                req.document_version_id,
                req.owner_type.as_str(),
                req.owner_id,
                input.claim_key,
                input.claim_kind.as_str(),
                input.source_type.as_str(),
                source_id,
                block_col,
                anchor_id,
                saved_quote,
                snapshot,
                structural.as_str(),
                support_status.as_str(),
                support_source.as_str(),
                key,
                now_ms,
            ],
        )
        .map_err(|e| db_app_error(&e))?;
        let row =
            load_citation_tx(conn, &new_ids[idx])?.ok_or_else(|| err(CoreErrorCode::Internal))?;
        out.push(row.to_view());
    }
    Ok(out)
}

fn load_citation_by_key_tx(
    conn: &Connection,
    idempotency_key: &str,
) -> Result<Option<CitationRow>, AppError> {
    conn.query_row(
        &format!("SELECT {CITATION_COLS} FROM citation WHERE idempotency_key=?1"),
        [idempotency_key],
        |r| CitationRow::try_from(r),
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

// —— 选区形状校验 ——

fn validate_selection_shape(sel: &RawSelectionInput) -> Result<(), AppError> {
    if sel.blocks.len() > MAX_ANCHOR_BLOCKS {
        return Err(err(CoreErrorCode::InvalidRequest)
            .with_detail("reason", "anchor_block_limit".into())
            .with_detail("max", (MAX_ANCHOR_BLOCKS as i64).into()));
    }
    // 空块选区只允许页面锚点(定位代替文本)。
    if sel.blocks.is_empty() && sel.primary_locator.pdf_page().is_none() {
        return Err(
            err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_selection".into())
        );
    }
    for b in &sel.blocks {
        if b.text.trim().is_empty() {
            return Err(
                err(CoreErrorCode::InvalidRequest).with_detail("reason", "empty_block_text".into())
            );
        }
    }
    // 选区文本必须等于块文本按原文顺序拼接(M03 约定 "\n" 连接);
    // 不把未匹配文本伪造为块引文(《M05》§2)。
    if !sel.blocks.is_empty() {
        let mut ordered: Vec<&RawSelectionBlockInput> = sel.blocks.iter().collect();
        ordered.sort_by_key(|b| b.block_order);
        let joined = ordered
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if joined != sel.text {
            return Err(err(CoreErrorCode::InvalidRequest)
                .with_detail("reason", "selection_text_mismatch".into()));
        }
    }
    Ok(())
}

// —— 重定位候选生成(《M05》§7)——

/// 目标代次解析块(只读)。
fn load_current_generation_blocks_tx(
    conn: &Connection,
    version_id: &str,
) -> Result<Vec<BlockRefRow>, AppError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {BLOCK_COLS} FROM content_block \
             WHERE parse_generation_id = \
               (SELECT id FROM parse_generation WHERE document_version_id=?1 AND is_current=1) \
             AND origin IN ('parser','ocr') ORDER BY ordinal"
        ))
        .map_err(|e| db_app_error(&e))?;
    let rows = stmt
        .query_map([version_id], |r| {
            Ok(BlockRefRow {
                id: r.get("id")?,
                document_version_id: r.get("document_version_id")?,
                parse_generation_id: r.get("parse_generation_id")?,
                origin: r.get("origin")?,
                block_key: r.get("block_key")?,
                ordinal: r.get("ordinal")?,
                kind: r.get("kind")?,
                text: r.get("text")?,
                heading_path_json: r.get("heading_path_json")?,
                locator_json: r.get("locator_json")?,
                indexable: r.get::<_, i64>("indexable")? != 0,
            })
        })
        .map_err(|e| db_app_error(&e))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| db_app_error(&e))?;
    Ok(rows)
}

/// 候选 JSON 持久化形态。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RelocationCandidateJson {
    anchor_id: String,
    target_version_id: String,
    score: f64,
    mappings: Vec<RelocationBlockMapping>,
    diff_note: String,
}

impl RelocationCandidateJson {
    fn from_candidate(c: &RelocationCandidate) -> Self {
        Self {
            anchor_id: c.anchor_id.clone(),
            target_version_id: c.target_version_id.clone(),
            score: c.score,
            mappings: c.mappings.clone(),
            diff_note: c.diff_note.clone(),
        }
    }
}

/// 加载重定位行(含确认后产生的新锚点 ID)。
pub(crate) fn load_relocation_tx(
    conn: &Connection,
    candidate_id: &str,
) -> Result<Option<RelocationCandidate>, AppError> {
    let row = load_relocation_row_tx(conn, candidate_id)?;
    row.map(|r| {
        let confirmed_anchor: Option<String> = if r.status == RelocationStatus::Confirmed {
            conn.query_row(
                "SELECT id FROM anchor WHERE idempotency_key=?1",
                [format!("relocation:{}", r.id)],
                |x| x.get(0),
            )
            .optional()
            .map_err(|e| db_app_error(&e))?
        } else {
            None
        };
        let json: Option<RelocationCandidateJson> = serde_json::from_str(&r.candidate_json).ok();
        let (mappings, diff_note) = match json {
            Some(j) => (j.mappings, j.diff_note),
            None => (Vec::new(), String::new()),
        };
        Ok(RelocationCandidate {
            id: r.id,
            anchor_id: r.anchor_id,
            target_version_id: r.target_version_id,
            score: r.score,
            status: r.status,
            mappings,
            diff_note,
            confirmed_anchor_id: confirmed_anchor,
            created_at: r.created_at,
        })
    })
    .transpose()
}

fn load_relocation_row_tx(
    conn: &Connection,
    candidate_id: &str,
) -> Result<Option<RelocationRow>, AppError> {
    conn.query_row(
        "SELECT id, anchor_id, target_version_id, candidate_json, score, status, \
         created_at, confirmed_at FROM anchor_relocation WHERE id=?1",
        [candidate_id],
        |r| {
            Ok(RelocationRow {
                id: r.get(0)?,
                anchor_id: r.get(1)?,
                target_version_id: r.get(2)?,
                candidate_json: r.get(3)?,
                score: r.get(4)?,
                status: RelocationStatus::parse_db(&r.get::<_, String>(5)?)
                    .unwrap_or(RelocationStatus::Proposed),
                created_at: r.get(6)?,
                confirmed_at: r.get(7)?,
            })
        },
    )
    .optional()
    .map_err(|e| db_app_error(&e))
}

fn row_to_candidate_view(row: &RelocationRow) -> RelocationCandidate {
    let json: Option<RelocationCandidateJson> = serde_json::from_str(&row.candidate_json).ok();
    let (mappings, diff_note) = match json {
        Some(j) => (j.mappings, j.diff_note),
        None => (Vec::new(), String::new()),
    };
    RelocationCandidate {
        id: row.id.clone(),
        anchor_id: row.anchor_id.clone(),
        target_version_id: row.target_version_id.clone(),
        score: row.score,
        status: row.status,
        mappings,
        diff_note,
        confirmed_anchor_id: None,
        created_at: row.created_at,
    }
}

fn best_block_match(
    quote: &str,
    quote_heading: Option<&Vec<String>>,
    targets: &[BlockRefRow],
) -> Option<(usize, f64, RelocationMethod, String)> {
    let quote_norm_hash = sha256_str(&normalize_v1(quote).normalized);
    let mut best: Option<(usize, f64, RelocationMethod, String)> = None;
    for (i, t) in targets.iter().enumerate() {
        let method = if t.text == quote {
            RelocationMethod::Exact
        } else if sha256_str(&normalize_v1(&t.text).normalized) == quote_norm_hash {
            RelocationMethod::Normalized
        } else if t.text.contains(quote) {
            RelocationMethod::Contains
        } else if match_quote_in_text(&t.text, quote).is_some() {
            RelocationMethod::NormalizedContains
        } else {
            continue;
        };
        let mut score: f64 = match method {
            RelocationMethod::Exact => 1.0,
            RelocationMethod::Normalized => 0.9,
            RelocationMethod::Contains => 0.7,
            RelocationMethod::NormalizedContains => 0.6,
        };
        // 标题路径一致加分(候选依据之一)。
        if let Some(hp) = quote_heading {
            let target_hp: Vec<String> =
                serde_json::from_str(&t.heading_path_json).unwrap_or_default();
            if !hp.is_empty() && hp == &target_hp {
                score = (score + 0.05).min(1.0);
            }
        }
        let matched = match method {
            RelocationMethod::Exact => quote.to_string(),
            _ => match_quote_in_text(&t.text, quote)
                .map(|(saved, _, _)| saved)
                .unwrap_or_else(|| quote.to_string()),
        };
        if best.as_ref().is_none_or(|b| score > b.1) {
            best = Some((i, score, method, matched));
        }
    }
    best
}

fn build_relocation_candidates(
    anchor: &AnchorRow,
    anchor_blocks: &[AnchorBlockRow],
    targets: &[BlockRefRow],
    target_version_id: &str,
) -> Vec<RelocationCandidate> {
    if anchor_blocks.is_empty() || targets.is_empty() {
        return Vec::new();
    }
    // 逐块最优匹配;要求全部命中且目标 ordinal 单调(原文顺序保持)。
    let mut mappings = Vec::new();
    let mut last_ordinal = i64::MIN;
    let mut total = 0.0;
    let mut worst = RelocationMethod::Exact;
    for ab in anchor_blocks {
        let Some((ti, score, method, matched)) =
            best_block_match(&ab.block_quote_text, None, targets)
        else {
            return Vec::new();
        };
        let t = &targets[ti];
        let ordinal = t.ordinal.unwrap_or(0);
        if ordinal <= last_ordinal {
            return Vec::new();
        }
        last_ordinal = ordinal;
        total += score;
        if method != RelocationMethod::Exact {
            worst = method;
        }
        mappings.push(RelocationBlockMapping {
            from_block_id: ab.block_id.clone(),
            to_block_id: t.id.clone(),
            to_block_key: t.block_key.clone(),
            to_ordinal: ordinal,
            method,
            matched_text: matched,
        });
    }
    let score = total / mappings.len() as f64;
    vec![RelocationCandidate {
        id: format!("reloc-{}", crate::shared::new_trace_id()),
        anchor_id: anchor.id.clone(),
        target_version_id: target_version_id.to_string(),
        score,
        status: RelocationStatus::Proposed,
        mappings,
        diff_note: worst.diff_note().to_string(),
        confirmed_anchor_id: None,
        created_at: 0,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_status_derives_orphaned_when_version_behind() {
        assert_eq!(
            effective_anchor_status(AnchorStatus::Active, "v1", Some("v2")),
            AnchorStatus::Orphaned
        );
        assert_eq!(
            effective_anchor_status(AnchorStatus::Active, "v1", Some("v1")),
            AnchorStatus::Active
        );
        assert_eq!(
            effective_anchor_status(AnchorStatus::Invalid, "v1", Some("v1")),
            AnchorStatus::Invalid
        );
        assert_eq!(
            effective_anchor_status(AnchorStatus::Relocated, "v1", None),
            AnchorStatus::Orphaned
        );
    }

    #[test]
    fn match_quote_prefers_exact_then_trim_then_normalized() {
        let block = "第一章  内容。\n第二段";
        // 精确。
        let (saved, range, norm) = match_quote_in_text(block, "内容。").unwrap();
        assert_eq!(saved, "内容。");
        assert!(range.is_some());
        assert!(!norm);
        // 省略首尾空白。
        let (saved, _, _) = match_quote_in_text(block, "  内容。 ").unwrap();
        assert_eq!(saved, "内容。");
        // 规范化:换行折叠 + 全角/半角。
        let block2 = "全角ＡＢ与软连字符";
        let (saved, _, norm) = match_quote_in_text(block2, "全角AB与软连字符").unwrap();
        assert_eq!(saved, "全角ＡＢ与软连字符");
        assert!(norm);
        // 软连字符:保存原文切片(含软连字符),不做规范化替换。
        let (saved, _, norm) =
            match_quote_in_text("Sys\u{00AD}tem design", "System design").unwrap();
        assert_eq!(saved, "Sys\u{00AD}tem design");
        assert!(norm);
        // 不存在 ⇒ None。
        assert!(match_quote_in_text(block, "不存在").is_none());
    }

    #[test]
    fn locator_resolves_checks_format_and_ranges() {
        let md = SourceLocator::markdown(0, 5, "paragraph", vec![]);
        assert!(locator_resolves(&md, DocumentFormat::Markdown));
        assert!(!locator_resolves(&md, DocumentFormat::Pdf));
        let bad_md = SourceLocator::markdown(9, 5, "paragraph", vec![]);
        assert!(!locator_resolves(&bad_md, DocumentFormat::Markdown));
        let pdf = SourceLocator::pdf(
            1,
            vec![crate::shared::locator::PdfRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 2.0,
            }],
            crate::shared::locator::PageRotation::R0,
        );
        assert!(locator_resolves(&pdf, DocumentFormat::Pdf));
    }
}
