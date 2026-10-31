//! M12 图服务(《M12》§3–§8)。
//!
//! 关键不变量:
//! - 生成只通过 M09 StructuredGenerationPort(M09 落地后由 M13 接线);
//!   M12 自身不调用模型。
//! - Artifact 身份、版本保护与保存由 M10 执行;M12 通过本模块定义的
//!   `GraphVersionSource` 端口读取待导出版本(M10 落地后由 M13 注入
//!   真实实现;假实现只存在于测试,《00》§1)。
//! - 布局失败/超限自动降级文本大纲,不产出空白产物;降级状态写入
//!   format 元数据(《M12》§6)。
//! - 大纲证据位置通过对 M04 `content_block` 的单表只读查询获得
//!   (与 M05 跨模块只读先例一致),不写他模块表。
//! - 导出已存在目标不静默覆盖;失败返回明确错误;结果不含完整路径。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use rusqlite::Connection;
use rusqlite::OptionalExtension;

use crate::exporters::graph::serialize_svg;
use crate::graph::layout::compute_layout;
use crate::graph::models::*;
use crate::graph::outline::{build_outline, EvidenceIndex};
use crate::graph::validate::validate_graph;
use crate::persistence::{db_app_error, Db};
use crate::shared::locator::SourceLocator;
use crate::shared::{AppError, CoreErrorCode};

use super::models::graph_error;

/// M10 产物版本读取端口:返回待渲染/导出的图内容。
/// M10 落地后由 M13 注入真实实现(artifact + artifact_version 单行读取)。
pub trait GraphVersionSource: Send + Sync {
    fn load_graph_version(
        &self,
        artifact_id: &str,
        version_id: &str,
    ) -> Result<GraphVersionContent, AppError>;
}

/// 端口返回的最小版本投影(不携带 M10 内部字段)。
#[derive(Debug, Clone)]
pub struct GraphVersionContent {
    pub document_id: String,
    pub content: GraphContent,
}

#[derive(Debug)]
pub struct GraphExportRequest {
    pub artifact_id: String,
    pub version_id: String,
    pub format: GraphExportFormat,
    /// 用户显式选择的目标路径(前端对话框提供)。
    pub path: std::path::PathBuf,
    pub idempotency_key: String,
}

pub struct GraphService {
    db: Arc<Db>,
    version_source: RwLock<Option<Arc<dyn GraphVersionSource>>>,
}

fn err(code: CoreErrorCode) -> AppError {
    AppError::new(code, crate::shared::new_trace_id())
}

impl GraphService {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            db,
            version_source: RwLock::new(None),
        }
    }

    /// 注册版本读取端口(M13 组装时接线;测试注入假实现)。
    pub fn register_version_source(&self, source: Arc<dyn GraphVersionSource>) {
        self.version_source
            .write()
            .expect("graph version source lock")
            .replace(source);
    }

    fn version_source(&self) -> Option<Arc<dyn GraphVersionSource>> {
        self.version_source
            .read()
            .expect("graph version source lock")
            .clone()
    }

    /// `graph_validate`:结构校验 + 证据规范化(纯函数,无数据库)。
    pub fn validate(&self, content: &GraphContent) -> GraphValidationReport {
        validate_graph(content)
    }

    /// `graph_layout`:计算布局;超限/环占优返回降级原因(不落库)。
    pub fn layout(&self, content: &GraphContent) -> GraphLayoutOutcome {
        compute_layout(content)
    }

    /// `graph_outline`:生成文本大纲。证据位置由 content_block 只读查询
    /// 获得(第 X 页/第 X 段);块缺失时退化为"原文块(未定位)"。
    pub fn outline(&self, content: &GraphContent) -> Result<GraphOutline, AppError> {
        let mut block_ids: Vec<String> = Vec::new();
        for node in &content.nodes {
            for cit in &node.citations {
                if !block_ids.contains(&cit.block_id) {
                    block_ids.push(cit.block_id.clone());
                }
            }
        }
        for edge in &content.edges {
            for cit in &edge.citations {
                if !block_ids.contains(&cit.block_id) {
                    block_ids.push(cit.block_id.clone());
                }
            }
        }
        let evidence = self
            .db
            .read(move |conn| load_evidence_index_tx(conn, &block_ids))?;
        Ok(build_outline(content, &evidence))
    }

    /// 渲染 SVG:结构必须有效且布局成功;布局降级时返回明确错误
    /// (调用方可改导出大纲,《M12》§7)。
    pub fn render_svg(&self, content: &GraphContent) -> Result<String, AppError> {
        let report = validate_graph(content);
        if !report.valid {
            return Err(graph_error(
                GraphErrorCode::GraphSchemaInvalid,
                &crate::shared::new_trace_id(),
            )
            .with_detail("reason", "structure_invalid".into()));
        }
        let normalized = report.normalized.expect("valid report carries normalized");
        let outcome = compute_layout(&normalized);
        let Some(layout) = outcome.layout else {
            return Err(graph_error(
                GraphErrorCode::GraphLayoutFailed,
                &crate::shared::new_trace_id(),
            )
            .with_detail(
                "reason",
                outcome
                    .fallback_reason
                    .unwrap_or_else(|| "unknown".into())
                    .into(),
            ));
        };
        Ok(serialize_svg(&normalized, &layout))
    }

    /// `graph_export`:读取产物版本(端口)→ 校验 → 序列化 → 写入。
    /// 目标已存在拒绝;写入失败明确报错;不静默覆盖(《M12》§7)。
    pub fn export(&self, req: &GraphExportRequest) -> Result<GraphExportResult, AppError> {
        if req.idempotency_key.trim().is_empty() {
            return Err(err(CoreErrorCode::InvalidRequest));
        }
        let source = self.version_source().ok_or_else(|| {
            graph_error(
                GraphErrorCode::GraphVersionSourceUnavailable,
                &crate::shared::new_trace_id(),
            )
        })?;
        let version = source.load_graph_version(&req.artifact_id, &req.version_id)?;
        let payload: String = match req.format {
            GraphExportFormat::Svg => self.render_svg(&version.content)?,
            GraphExportFormat::Outline => {
                let outline = self.outline(&version.content)?;
                outline.text
            }
        };
        // 目标已存在:拒绝,不静默覆盖(路径本身不进入错误详情)。
        if req.path.exists() {
            return Err(graph_error(
                GraphErrorCode::GraphExportTargetExists,
                &crate::shared::new_trace_id(),
            ));
        }
        let bytes = payload.len() as i64;
        std::fs::write(&req.path, payload.as_bytes()).map_err(|e| {
            graph_error(
                GraphErrorCode::GraphExportFailed,
                &crate::shared::new_trace_id(),
            )
            .with_detail("reason", e.kind().to_string().into())
        })?;
        Ok(GraphExportResult {
            format: req.format,
            bytes_written: bytes,
        })
    }
}

/// 证据位置索引:content_block 单表只读查询(M04,跨模块只读先例)。
fn load_evidence_index_tx(
    conn: &Connection,
    block_ids: &[String],
) -> Result<EvidenceIndex, AppError> {
    let mut index: EvidenceIndex = HashMap::new();
    for id in block_ids {
        let row: Option<(Option<i64>, String, String)> = conn
            .query_row(
                "SELECT ordinal, locator_json, heading_path_json FROM content_block WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| db_app_error(&e))?;
        let Some((ordinal, locator_json, heading_path_json)) = row else {
            continue; // 块缺失(解析缓存清理):大纲显示"未定位",不伪造位置
        };
        let locator: Option<SourceLocator> = serde_json::from_str(&locator_json).ok();
        index.insert(
            id.clone(),
            EvidencePosition {
                block_id: id.clone(),
                page_number: locator.as_ref().and_then(|l| l.pdf_page()),
                ordinal,
                heading_path: serde_json::from_str(&heading_path_json).ok(),
            },
        );
    }
    Ok(index)
}
