//! 派生数据清除协调(《M08》§8 / 《01》§11 清除矩阵)。
//!
//! 流程:取消相关运行任务 → 收集 Contributor 预览 → M05 证据保留裁决 →
//! 生成带 planId/planHash/过期时间的清计划 → 用户二次确认 → 执行前复核
//! (计划过期、哈希不符、预览后数据漂移均拒绝)→ 按序执行 → 写非内容审计。
//!
//! 边界:M08 只协调,不跨模块写业务表——各类别的预览与删除由拥有该数据的
//! 模块以 Contributor 注册进来(M06 已接线;M07 会话/M10 产物/M04 索引
//! 缓存在其提供清除接口后注册,未注册类别在预览中如实标记不可用)。
//! 原文文件永不进入清除;隐私清除不创建正文备份。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::evidence::models::{ProtectedBlockView, RetentionCategory, RetentionRuling};
use crate::notes::models::{UserdataClearCategory, UserdataClearPreview};
use crate::notes::service::NotesService;
use crate::persistence::{db_app_error, Db};
use crate::shared::events::{DomainEvent, DomainEventType, EventSink};
use crate::shared::textnorm::sha256_hex;
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::errors::{model_err, ModelErrorCode};
use super::grants::document_exists_tx;
use super::models::{
    ClearCategory, ClearCategoryExecution, ClearCategoryPreview, ClearPreview, ClearResult,
    ClearUnavailableReason, CLEAR_PLAN_TTL_MS, PRIVACY_POLICY_VERSION,
};

/// 清除贡献器端口:拥有某类派生数据的模块实现预览与删除。
/// 执行必须自行保证事务性;M08 在执行前完成计划复核与任务取消。
pub trait DataClearContributor: Send + Sync {
    fn contributor_id(&self) -> &'static str;
    fn supported_categories(&self) -> &'static [ClearCategory];
    fn preview(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<Vec<ClearCategoryPreview>, AppError>;
    fn execute(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<Vec<ClearCategoryExecution>, AppError>;
}

/// M05 证据保留裁决端口(M08 清理解析缓存类数据前必须咨询)。
pub trait RetentionAdvisor: Send + Sync {
    fn advise(
        &self,
        document_id: &str,
        categories: &[RetentionCategory],
    ) -> Result<RetentionRuling, AppError>;
}

/// 执行前取消活动任务的端口(TaskService 适配)。
pub trait TaskCancelPort: Send + Sync {
    /// 请求取消文档的全部活动任务;返回请求数(queued 即 canceled)。
    fn cancel_active_for_document(&self, document_id: &str) -> Result<i64, AppError>;
}

// —— M05/M06 接线(组合根注入;M07/M10/M04 接口就绪后同样注册)——

/// M05 接线:保留裁决只读,不删除(M05 不执行删除)。
pub struct EvidenceRetentionAdvisor {
    evidence: Arc<crate::evidence::service::EvidenceService>,
}

impl EvidenceRetentionAdvisor {
    pub fn new(evidence: Arc<crate::evidence::service::EvidenceService>) -> Self {
        Self { evidence }
    }
}

impl RetentionAdvisor for EvidenceRetentionAdvisor {
    fn advise(
        &self,
        document_id: &str,
        categories: &[RetentionCategory],
    ) -> Result<RetentionRuling, AppError> {
        self.evidence
            .evaluate_evidence_retention(document_id, categories)
    }
}

/// M06 接线:标注/书签/笔记/待澄清/标签的预览与删除。
pub struct NotesClearContributor {
    notes: Arc<NotesService>,
}

impl NotesClearContributor {
    pub fn new(notes: Arc<NotesService>) -> Self {
        Self { notes }
    }
}

const NOTES_CATEGORIES: [ClearCategory; 5] = [
    ClearCategory::Annotations,
    ClearCategory::Bookmarks,
    ClearCategory::Notes,
    ClearCategory::Clarifications,
    ClearCategory::Tags,
];

fn userdata_category_of(c: ClearCategory) -> Option<UserdataClearCategory> {
    Some(match c {
        ClearCategory::Annotations => UserdataClearCategory::Annotations,
        ClearCategory::Bookmarks => UserdataClearCategory::Bookmarks,
        ClearCategory::Notes => UserdataClearCategory::Notes,
        ClearCategory::Clarifications => UserdataClearCategory::Clarifications,
        ClearCategory::Tags => UserdataClearCategory::Tags,
        _ => return None,
    })
}

fn clear_category_of_userdata(c: UserdataClearCategory) -> ClearCategory {
    match c {
        UserdataClearCategory::Annotations => ClearCategory::Annotations,
        UserdataClearCategory::Bookmarks => ClearCategory::Bookmarks,
        UserdataClearCategory::Notes => ClearCategory::Notes,
        UserdataClearCategory::Clarifications => ClearCategory::Clarifications,
        UserdataClearCategory::Tags => ClearCategory::Tags,
    }
}

impl DataClearContributor for NotesClearContributor {
    fn contributor_id(&self) -> &'static str {
        "m06_userdata"
    }
    fn supported_categories(&self) -> &'static [ClearCategory] {
        &NOTES_CATEGORIES
    }
    fn preview(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<Vec<ClearCategoryPreview>, AppError> {
        let userdata: Vec<UserdataClearCategory> = categories
            .iter()
            .filter_map(|c| userdata_category_of(*c))
            .collect();
        let UserdataClearPreview {
            categories: counts, ..
        } = self.notes.preview_clear(document_id, &userdata)?;
        Ok(counts
            .into_iter()
            .map(|c| ClearCategoryPreview {
                category: clear_category_of_userdata(c.category),
                available: true,
                unavailable_reason: None,
                count: Some(c.count),
                affected_citations: Some(c.affected_citations),
                protected_count: None,
            })
            .collect())
    }
    fn execute(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<Vec<ClearCategoryExecution>, AppError> {
        let userdata: Vec<UserdataClearCategory> = categories
            .iter()
            .filter_map(|c| userdata_category_of(*c))
            .collect();
        let result = self.notes.execute_clear(document_id, &userdata)?;
        Ok(result
            .deleted
            .into_iter()
            .map(|c| ClearCategoryExecution {
                category: clear_category_of_userdata(c.category),
                deleted: c.count,
            })
            .collect())
    }
}

// —— 协调器 ——

struct StoredPlan {
    document_id: String,
    categories: Vec<ClearCategory>,
    plan_hash: String,
    created_at: i64,
    expires_at: i64,
    /// 预览快照(执行后比对"预览与实际一致"用)。
    previews: Vec<ClearCategoryPreview>,
}

pub struct ClearCoordinator {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    events: Arc<dyn EventSink>,
    contributors: RwLock<Vec<Arc<dyn DataClearContributor>>>,
    retention: RwLock<Option<Arc<dyn RetentionAdvisor>>>,
    task_canceller: RwLock<Option<Arc<dyn TaskCancelPort>>>,
    plans: Mutex<HashMap<String, StoredPlan>>,
}

fn err_invalid(reason: &'static str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, crate::shared::new_trace_id())
        .with_detail("reason", reason.into())
}

/// 键排序的规范 JSON(计划哈希的确定性基础)。
pub(crate) fn canonical_json(value: &serde_json::Value) -> String {
    fn write_sorted(v: &serde_json::Value, out: &mut String) {
        match v {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(k).unwrap_or_default());
                    out.push(':');
                    write_sorted(&map[*k], out);
                }
                out.push('}');
            }
            serde_json::Value::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_sorted(item, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    write_sorted(value, &mut out);
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlanHashInput<'a> {
    document_id: &'a str,
    categories: Vec<serde_json::Value>,
    protected_blocks: Vec<&'a str>,
    policy_version: &'a str,
}

/// 计划哈希只绑定预览内容(类别计数与受保护块),不含时间与 planId:
/// 同内容预览哈希稳定,执行前重算才能检测"预览后数据漂移"。
fn compute_plan_hash(
    document_id: &str,
    previews: &[ClearCategoryPreview],
    protected: &[ProtectedBlockView],
) -> String {
    let mut categories: Vec<serde_json::Value> = previews
        .iter()
        .map(|p| {
            serde_json::json!({
                "category": p.category.as_str(),
                "available": p.available,
                "count": p.count,
                "affectedCitations": p.affected_citations,
                "protectedCount": p.protected_count,
            })
        })
        .collect();
    categories.sort_by(|a, b| a["category"].as_str().cmp(&b["category"].as_str()));
    let mut blocks: Vec<&str> = protected.iter().map(|b| b.block_id.as_str()).collect();
    blocks.sort();
    let input = PlanHashInput {
        document_id,
        categories,
        protected_blocks: blocks,
        policy_version: PRIVACY_POLICY_VERSION,
    };
    let value = serde_json::to_value(&input).unwrap_or(serde_json::Value::Null);
    sha256_hex(canonical_json(&value).as_bytes())
}

impl ClearCoordinator {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            events,
            contributors: RwLock::new(Vec::new()),
            retention: RwLock::new(None),
            task_canceller: RwLock::new(None),
            plans: Mutex::new(HashMap::new()),
        }
    }

    pub fn register_contributor(&self, contributor: Arc<dyn DataClearContributor>) {
        self.contributors.write().unwrap().push(contributor);
    }

    pub fn set_retention_advisor(&self, advisor: Arc<dyn RetentionAdvisor>) {
        *self.retention.write().unwrap() = Some(advisor);
    }

    pub fn set_task_canceller(&self, port: Arc<dyn TaskCancelPort>) {
        *self.task_canceller.write().unwrap() = Some(port);
    }

    fn find_contributor(&self, category: ClearCategory) -> Option<Arc<dyn DataClearContributor>> {
        self.contributors
            .read()
            .unwrap()
            .iter()
            .find(|c| c.supported_categories().contains(&category))
            .cloned()
    }

    /// M05 裁决 → 索引/缓存类预览(执行依赖对应模块注册贡献器;
    /// 未注册时如实标记不可用,但展示裁决计数与受保护块)。
    fn index_categories_preview(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<(Vec<ClearCategoryPreview>, Vec<ProtectedBlockView>), AppError> {
        let mut previews = Vec::new();
        let mut protected_blocks = Vec::new();
        let advisor = self.retention.read().unwrap().clone();
        let needs_ruling = categories
            .iter()
            .any(|c| matches!(c, ClearCategory::FtsVectorCache | ClearCategory::ParseCache));
        let mut ruling: Option<RetentionRuling> = None;
        if needs_ruling {
            if let Some(advisor) = advisor {
                ruling = Some(advisor.advise(
                    document_id,
                    &[
                        RetentionCategory::FtsIndex,
                        RetentionCategory::VectorIndex,
                        RetentionCategory::ParseCache,
                    ],
                )?);
            }
        }
        for category in categories {
            match category {
                ClearCategory::FtsVectorCache | ClearCategory::ParseCache => {
                    let mut count = None;
                    let mut protected_count = None;
                    if let Some(r) = &ruling {
                        let (c, p) = match category {
                            ClearCategory::FtsVectorCache => {
                                let c: i64 = r
                                    .categories
                                    .iter()
                                    .filter(|x| {
                                        matches!(
                                            x.category,
                                            RetentionCategory::FtsIndex
                                                | RetentionCategory::VectorIndex
                                        )
                                    })
                                    .map(|x| x.cleanable_count)
                                    .sum();
                                let p: i64 = r
                                    .categories
                                    .iter()
                                    .filter(|x| {
                                        matches!(
                                            x.category,
                                            RetentionCategory::FtsIndex
                                                | RetentionCategory::VectorIndex
                                        )
                                    })
                                    .map(|x| x.protected_count)
                                    .sum();
                                (c, p)
                            }
                            _ => r
                                .categories
                                .iter()
                                .find(|x| matches!(x.category, RetentionCategory::ParseCache))
                                .map(|x| (x.cleanable_count, x.protected_count))
                                .unwrap_or((0, 0)),
                        };
                        count = Some(c);
                        protected_count = Some(p);
                    }
                    if let Some(r) = &ruling {
                        protected_blocks.extend(r.protected_blocks.clone());
                    }
                    previews.push(ClearCategoryPreview {
                        category: *category,
                        available: self.find_contributor(*category).is_some(),
                        unavailable_reason: self
                            .find_contributor(*category)
                            .is_none()
                            .then_some(ClearUnavailableReason::ContributorNotRegistered),
                        count,
                        affected_citations: None,
                        protected_count,
                    });
                }
                _ => {}
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        protected_blocks.retain(|b| seen.insert(b.block_id.clone()));
        protected_blocks.sort_by(|a, b| a.block_id.cmp(&b.block_id));
        Ok((previews, protected_blocks))
    }

    /// 生成清计划(内存注册;planHash 绑定预览快照)。
    pub fn preview_clear(
        &self,
        document_id: &str,
        categories: &[ClearCategory],
    ) -> Result<ClearPreview, AppError> {
        let did = document_id.to_string();
        self.db.read(move |conn| {
            if !document_exists_tx(conn, &did)? {
                return Err(AppError::new(
                    CoreErrorCode::DocNotFound,
                    crate::shared::new_trace_id(),
                ));
            }
            Ok(())
        })?;
        // 规范化:去重并按固定类别顺序排列。
        let cats: Vec<ClearCategory> = ClearCategory::ALL
            .into_iter()
            .filter(|c| categories.contains(c))
            .collect();
        if cats.is_empty() {
            return Err(err_invalid("no_categories"));
        }
        let now = self.clock.now_ms();
        // 有贡献器的类别:预览计数。
        let mut previews: Vec<ClearCategoryPreview> = Vec::new();
        let mut by_contributor: Vec<(Arc<dyn DataClearContributor>, Vec<ClearCategory>)> =
            Vec::new();
        for cat in &cats {
            if let Some(contributor) = self.find_contributor(*cat) {
                match by_contributor
                    .iter_mut()
                    .find(|(c, _)| c.contributor_id() == contributor.contributor_id())
                {
                    Some((_, list)) => list.push(*cat),
                    None => by_contributor.push((contributor, vec![*cat])),
                }
            }
        }
        for (contributor, cats_for) in &by_contributor {
            previews.extend(contributor.preview(document_id, cats_for)?);
        }
        // 索引/缓存类:M05 裁决计数(可能无执行器)。
        let index_cats: Vec<ClearCategory> = cats
            .iter()
            .copied()
            .filter(|c| matches!(c, ClearCategory::FtsVectorCache | ClearCategory::ParseCache))
            .collect();
        let mut protected_blocks = Vec::new();
        if !index_cats.is_empty() {
            let (mut idx_previews, blocks) =
                self.index_categories_preview(document_id, &index_cats)?;
            previews.append(&mut idx_previews);
            protected_blocks = blocks;
        }
        // 其余未注册类别(会话/产物)如实标记。
        for cat in &cats {
            if !previews.iter().any(|p| p.category == *cat) {
                previews.push(ClearCategoryPreview {
                    category: *cat,
                    available: false,
                    unavailable_reason: Some(ClearUnavailableReason::ContributorNotRegistered),
                    count: None,
                    affected_citations: None,
                    protected_count: None,
                });
            }
        }
        previews.sort_by_key(|p| ClearCategory::ALL.iter().position(|c| *c == p.category));
        let plan_id = self.ids.new_entity_id();
        let plan_hash = compute_plan_hash(document_id, &previews, &protected_blocks);
        let expires_at = now + CLEAR_PLAN_TTL_MS;
        let requires_explicit_confirmation = cats.iter().any(|c| c.is_user_asset());
        let preview = ClearPreview {
            schema_version: "clear_preview.v1",
            document_id: document_id.to_string(),
            plan_id: plan_id.clone(),
            plan_hash: plan_hash.clone(),
            expires_at,
            categories: previews.clone(),
            protected_blocks: protected_blocks.clone(),
            original_file_affected: false,
            requires_explicit_confirmation,
        };
        // 注册计划并清理过期项(内存;最多保留 32 个近期计划)。
        {
            let mut plans = self.plans.lock().unwrap();
            plans.retain(|_, p| p.expires_at > now);
            while plans.len() >= 32 {
                if let Some(oldest) = plans
                    .keys()
                    .min_by_key(|k| plans.get(*k).map(|p| p.created_at).unwrap_or(i64::MAX))
                    .cloned()
                {
                    plans.remove(&oldest);
                } else {
                    break;
                }
            }
            plans.insert(
                plan_id,
                StoredPlan {
                    document_id: document_id.to_string(),
                    categories: cats.clone(),
                    plan_hash: plan_hash.clone(),
                    created_at: now,
                    expires_at,
                    previews,
                },
            );
        }
        Ok(preview)
    }

    /// 执行清计划:全量复核(存在/归属/过期/哈希/漂移/确认)→ 取消任务 →
    /// 按序执行贡献器 → 写审计。任何拒绝都零删除。
    pub fn execute_clear(
        &self,
        document_id: &str,
        plan_id: &str,
        plan_hash: &str,
        confirmation: &str,
        idempotency_key: &str,
    ) -> Result<ClearResult, AppError> {
        // 幂等:同 key 重放返回已记录结果(不再删除)。
        if let Some(replay) = self.load_audit_result(idempotency_key)? {
            return Ok(replay);
        }
        let now = self.clock.now_ms();
        let plan = {
            let plans = self.plans.lock().unwrap();
            plans.get(plan_id).cloned()
        };
        let Some(plan) = plan else {
            return Err(err_invalid("plan_unknown"));
        };
        if plan.document_id != document_id {
            return Err(err_invalid("plan_document_mismatch"));
        }
        if now >= plan.expires_at {
            return Err(model_err(ModelErrorCode::PrivacyPlanExpired));
        }
        if plan.plan_hash != plan_hash {
            return Err(err_invalid("plan_hash_mismatch"));
        }
        if confirmation != document_id {
            return Err(err_invalid("confirmation_mismatch"));
        }
        // 复核:重跑预览,数据漂移(哈希变化)则拒绝,要求重新预览。
        let fresh = self.preview_clear(document_id, &plan.categories)?;
        if fresh.plan_hash != plan.plan_hash {
            return Err(model_err(ModelErrorCode::PrivacyPlanExpired)
                .with_detail("reason", "preview_outdated".into()));
        }
        if fresh.categories.iter().any(|c| !c.available) {
            return Err(err_invalid("category_unavailable"));
        }
        // 取消相关运行任务(queued 即取消;running 请求取消)。
        let canceled_tasks = match self.task_canceller.read().unwrap().clone() {
            Some(port) => port.cancel_active_for_document(document_id)?,
            None => 0,
        };
        // 按类别规范顺序执行贡献器(每个贡献器一次,携带其类别子集)。
        let mut executed: Vec<ClearCategoryExecution> = Vec::new();
        let mut done: Vec<&'static str> = Vec::new();
        for cat in ClearCategory::ALL {
            if !plan.categories.contains(&cat) {
                continue;
            }
            if let Some(contributor) = self.find_contributor(cat) {
                if done.contains(&contributor.contributor_id()) {
                    continue;
                }
                let subset: Vec<ClearCategory> = plan
                    .categories
                    .iter()
                    .copied()
                    .filter(|c| contributor.supported_categories().contains(c))
                    .collect();
                executed.extend(contributor.execute(document_id, &subset)?);
                done.push(contributor.contributor_id());
            }
        }
        executed.sort_by_key(|e| ClearCategory::ALL.iter().position(|c| *c == e.category));
        let audit_id = self.ids.new_entity_id();
        let result = ClearResult {
            schema_version: "clear_result.v1".into(),
            document_id: document_id.to_string(),
            plan_id: plan_id.to_string(),
            executed: executed.clone(),
            canceled_tasks,
            audit_id: audit_id.clone(),
        };
        // 审计:非内容(类别+计数+状态);同事务写入失败即整体报错。
        let now2 = self.clock.now_ms();
        let did = document_id.to_string();
        let categories_json = serde_json::to_string(
            &plan
                .categories
                .iter()
                .map(|c| c.as_str())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|_| "[]".into());
        let result_json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".into());
        let key = idempotency_key.to_string();
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO data_clear_audit(id, document_id, categories_json, task_id, status, \
                 result_json, idempotency_key, created_at, completed_at) \
                 VALUES (?1,?2,?3,NULL,'completed',?4,?5,?6,?6)",
                params![audit_id, did, categories_json, result_json, key, now2],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })?;
        // 事件只在 COMMIT 后发出。
        let event = DomainEvent::new(
            DomainEventType::PrivacyChanged,
            document_id.to_string(),
            1,
            self.clock.now_ms(),
            serde_json::json!({
                "action": "clear_completed",
                "categories": plan.categories.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
            }),
        )
        .with_document(Some(document_id.to_string()), None);
        self.events.emit(event);
        Ok(result)
    }

    fn load_audit_result(&self, idempotency_key: &str) -> Result<Option<ClearResult>, AppError> {
        let key = idempotency_key.to_string();
        self.db.read(move |conn| {
            let result_json: Option<String> = conn
                .query_row(
                    "SELECT result_json FROM data_clear_audit WHERE idempotency_key=?1 \
                     AND status='completed'",
                    [&key],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| db_app_error(&e))?;
            match result_json {
                None => Ok(None),
                Some(json) => {
                    let parsed: ClearResult = serde_json::from_str(&json)
                        .map_err(|_| AppError::internal("clear-audit-parse"))?;
                    Ok(Some(parsed))
                }
            }
        })
    }
}

impl Clone for StoredPlan {
    fn clone(&self) -> Self {
        Self {
            document_id: self.document_id.clone(),
            categories: self.categories.clone(),
            plan_hash: self.plan_hash.clone(),
            created_at: self.created_at,
            expires_at: self.expires_at,
            previews: self.previews.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_deterministically() {
        let a = serde_json::json!({"b": 1, "a": {"z": 1, "c": 2}});
        let b = serde_json::json!({"a": {"c": 2, "z": 1}, "b": 1});
        assert_eq!(canonical_json(&a), canonical_json(&b));
        let c = serde_json::json!({"b": 1, "a": {"z": 2, "c": 2}});
        assert_ne!(canonical_json(&a), canonical_json(&c));
    }

    #[test]
    fn plan_hash_binds_counts_and_protection() {
        let previews = vec![ClearCategoryPreview {
            category: ClearCategory::Notes,
            available: true,
            unavailable_reason: None,
            count: Some(2),
            affected_citations: Some(1),
            protected_count: None,
        }];
        let blocks = vec![ProtectedBlockView {
            block_id: "b1".into(),
            reason: "anchor_reference".into(),
        }];
        let base = compute_plan_hash("d1", &previews, &blocks);
        // 同内容稳定(与时间无关,执行前复核依赖此性质)。
        assert_eq!(base, compute_plan_hash("d1", &previews, &blocks));
        let mut changed = previews.clone();
        changed[0].count = Some(3);
        assert_ne!(base, compute_plan_hash("d1", &changed, &blocks));
        let changed_blocks = vec![ProtectedBlockView {
            block_id: "b2".into(),
            reason: "anchor_reference".into(),
        }];
        assert_ne!(base, compute_plan_hash("d1", &previews, &changed_blocks));
        assert_ne!(base, compute_plan_hash("d2", &previews, &blocks));
    }
}
