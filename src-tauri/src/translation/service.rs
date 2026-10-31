//! 2C 翻译编排:占位符保护 → 翻译记忆 → 分批模型调用 → 服务端校验/置信度。

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::{params, OptionalExtension};

use crate::artifacts::models::{ArtifactView, CoverageRegion, CoverageReport};
use crate::artifacts::service::AiDraftCommitInput;
use crate::artifacts::ArtifactService;
use crate::context::models::ContextSnapshotView;
use crate::parse_index::models::ContentBlockView;
use crate::parse_index::ParseIndexService;
use crate::persistence::{db_app_error, Db};
use crate::retrieval_ai::models::{
    GenerationInput, SourceRole, StructuredGenerationRequest, TaskKind,
};
use crate::retrieval_ai::ports::StructuredGenerationPort;
use crate::shared::textnorm::sha256_str;
use crate::shared::{AppError, Clock, CoreErrorCode, IdGenerator};

use super::models::{
    GlossaryEntryV1, TranslationConfidence, TranslationCoverageV1, TranslationSegmentV1,
    TranslationStatus, TranslationV1, TRANSLATION_SCHEMA_VERSION,
};

pub const TRANSLATION_INSTRUCTION_VERSION: &str = "translation-instruction-v1";
const BATCH_SIZE: usize = 5;

pub struct TranslationService {
    db: Arc<Db>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
    parse: Arc<ParseIndexService>,
    generation: Arc<dyn StructuredGenerationPort>,
    artifacts: Arc<ArtifactService>,
}

impl TranslationService {
    pub fn new(
        db: Arc<Db>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
        parse: Arc<ParseIndexService>,
        generation: Arc<dyn StructuredGenerationPort>,
        artifacts: Arc<ArtifactService>,
    ) -> Self {
        Self {
            db,
            clock,
            ids,
            parse,
            generation,
            artifacts,
        }
    }

    pub fn translate(
        &self,
        document_id: &str,
        snapshot: &ContextSnapshotView,
        target_language: &str,
        style: Option<&str>,
        glossary: &[GlossaryEntryV1],
        title: Option<String>,
    ) -> Result<ArtifactView, AppError> {
        self.translate_with_cancel(
            document_id,
            snapshot,
            target_language,
            style,
            glossary,
            title,
            None,
            &tokio_util::sync::CancellationToken::new(),
        )
    }

    /// 带取消令牌的翻译:后台任务取消后不再发起后续批次模型调用,
    /// 已完成的段落保留在内存中但不落库(不保存半成品产物)。
    ///
    /// `task_id` 必须由后台任务传入:产物版本靠 `artifact_version.task_id`
    /// 与任务关联,`translation_get`/`find_artifact_by_task` 才能找回结果。
    /// 早先这里固定写 None,导致任务成功却永远查不到产物。
    #[allow(clippy::too_many_arguments)]
    pub fn translate_with_cancel(
        &self,
        document_id: &str,
        snapshot: &ContextSnapshotView,
        target_language: &str,
        style: Option<&str>,
        glossary: &[GlossaryEntryV1],
        title: Option<String>,
        task_id: Option<&str>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<ArtifactView, AppError> {
        if target_language.trim().is_empty() || target_language.chars().count() > 32 {
            return Err(invalid("invalid_target_language"));
        }
        let blocks = self.snapshot_blocks(snapshot)?;
        if blocks.is_empty() {
            return Err(invalid("translation_no_segments"));
        }
        let style = style.unwrap_or("faithful").to_string();
        let glossary = normalize_glossary(glossary);
        let glossary_version = sha256_str(
            &glossary
                .iter()
                .map(|entry| format!("{}={}", entry.source, entry.target))
                .collect::<Vec<_>>()
                .join("|"),
        );
        let engine_hint = "translation-role";
        type PendingSegment = (usize, String, Vec<(String, String)>);
        let mut pending: Vec<PendingSegment> = Vec::new();
        let mut segments: Vec<Option<TranslationSegmentV1>> = vec![None; blocks.len()];
        for (index, block) in blocks.iter().enumerate() {
            let text = block.text.trim();
            if text.is_empty() {
                continue;
            }
            let (masked, tokens) = protect(text);
            segments[index] = Some(TranslationSegmentV1 {
                id: format!("t{}", index + 1),
                source_block_id: block.id.clone(),
                page_number: block.locator.pdf_page(),
                source_text_hash: sha256_str(text),
                source_text: text.to_string(),
                translated_text: String::new(),
                status: TranslationStatus::Failed,
                scope: snapshot.scope_kind.as_str().to_string(),
                engine: engine_hint.to_string(),
                confidence: TranslationConfidence::Low,
                glossary_hits: Vec::new(),
                failure_reason: Some("not_translated".into()),
            });
            if !needs_translation(text) {
                if let Some(segment) = segments[index].as_mut() {
                    segment.translated_text = text.to_string();
                    segment.status = TranslationStatus::Translated;
                    segment.confidence = TranslationConfidence::High;
                    segment.engine = "passthrough".into();
                    segment.failure_reason = None;
                }
                continue;
            }
            if let Some(hit) = self.memory_get(
                &sha256_str(text),
                target_language,
                engine_hint,
                &glossary_version,
            )? {
                let restored = restore(&hit, &tokens);
                let confidence = confidence_of(text, &restored, &tokens, glossary.as_slice());
                let hits = glossary_hits(text, glossary.as_slice());
                if let Some(segment) = segments[index].as_mut() {
                    segment.translated_text = restored;
                    segment.status = status_of(confidence);
                    segment.confidence = confidence;
                    segment.glossary_hits = hits;
                    segment.failure_reason = None;
                    segment.engine = "memory".into();
                }
                continue;
            }
            pending.push((index, masked, tokens));
        }

        let mut last_error: Option<AppError> = None;
        for (batch_no, batch) in pending.chunks(BATCH_SIZE).enumerate() {
            // 批次之间检查取消:取消后不再发起模型调用,也不落任何产物。
            if cancel.is_cancelled() {
                return Err(cancellation_error());
            }
            // 每个输入使用自身 id 作为 source_id,便于模型回传。
            let inputs: Vec<GenerationInput> = batch
                .iter()
                .enumerate()
                .map(|(offset, (_, masked, _))| GenerationInput {
                    source_id: format!("t{}", batch_no * BATCH_SIZE + offset + 1),
                    role: SourceRole::Original,
                    text: masked.clone(),
                })
                .collect();
            let glossary_lines = glossary
                .iter()
                .map(|entry| format!("{} => {}", entry.source, entry.target))
                .collect::<Vec<_>>()
                .join("\n");
            let question = format!(
                "Translate each SOURCE segment into {target_language}, style {style}. \
                 Preserve every placeholder token like ⟦P0⟧ exactly in place. \
                 Do not translate code, formulas, URLs or emails (they are placeholders). \
                 Apply this glossary when the source term appears:\n{glossary_lines}\n\
                 Return JSON: {{\"translations\":[{{\"id\":\"SOURCE id\",\"text\":\"translated text\"}}]}} \
                 schemaVersion={TRANSLATION_SCHEMA_VERSION}."
            );
            let request = StructuredGenerationRequest {
                task_id: format!("translation-{}", sha256_str(&snapshot.id)),
                attempt_id: String::new(),
                document_id: document_id.to_string(),
                snapshot_id: snapshot.id.clone(),
                schema_version: TRANSLATION_SCHEMA_VERSION.to_string(),
                schema: translation_schema(),
                instruction_version: TRANSLATION_INSTRUCTION_VERSION.to_string(),
                task_kind: TaskKind::Translation,
                inputs,
                locale: target_language.to_string(),
                question,
                history: Vec::new(),
                history_summary: None,
                attachments: Vec::new(),
                document_map: None,
                // 翻译是快照级子步骤,没有独立 Task:只跳过任务门控,
                // 授权/载荷哈希/来源白名单/密钥/审计仍由网关校验。
                scope: crate::model_privacy::gateway::EgressScope::Document,
            };
            match self.generation.generate_translation(&request, cancel) {
                Ok(result) => {
                    let Ok(map) = parse_translations(&result.content) else {
                        for (index, _, _) in batch {
                            if let Some(segment) = segments.get_mut(*index).and_then(Option::as_mut)
                            {
                                keep_source(segment, "model_output_invalid");
                            }
                        }
                        continue;
                    };
                    for (offset, (index, _, tokens)) in batch.iter().enumerate() {
                        let id = format!("t{}", batch_no * BATCH_SIZE + offset + 1);
                        let Some(translated_masked) = lookup_translation(&map, &id) else {
                            if let Some(segment) = segments.get_mut(*index).and_then(Option::as_mut)
                            {
                                keep_source(segment, "segment_missing_in_response");
                            }
                            continue;
                        };
                        if let Some(segment) = segments.get_mut(*index).and_then(Option::as_mut) {
                            let restored = restore(translated_masked, tokens);
                            // 与内联翻译同一护栏:模型回显指令或原样返回原文都不是译文,
                            // 保留原文并记录稳定原因,不能把指令当译文存进产物。
                            if looks_like_instruction(&restored) {
                                keep_source(segment, "translation_echoed_instruction");
                                segment.engine = "model".into();
                                continue;
                            }
                            if translation_not_applied(&segment.source_text, &restored) {
                                keep_source(segment, "translation_not_applied");
                                segment.engine = "model".into();
                                continue;
                            }
                            let confidence =
                                confidence_of(&segment.source_text, &restored, tokens, &glossary);
                            let hits = glossary_hits(&segment.source_text, &glossary);
                            segment.translated_text = restored;
                            segment.confidence = confidence;
                            segment.status = status_of(confidence);
                            segment.glossary_hits = hits;
                            segment.failure_reason = if confidence == TranslationConfidence::Low {
                                Some("low_confidence".into())
                            } else {
                                None
                            };
                            segment.engine = "model".into();
                            let _ = self.memory_put(
                                &segment.source_text_hash,
                                target_language,
                                engine_hint,
                                &glossary_version,
                                &segment.translated_text,
                            );
                        }
                    }
                }
                Err(error) => {
                    last_error = Some(error);
                    for (index, _, _) in batch {
                        if let Some(segment) = segments.get_mut(*index).and_then(Option::as_mut) {
                            keep_source(segment, "model_unavailable");
                        }
                    }
                }
            }
        }

        let segments: Vec<TranslationSegmentV1> = segments.into_iter().flatten().collect();
        let translated = segments
            .iter()
            .filter(|s| s.status != TranslationStatus::Failed)
            .count() as i64;
        if translated == 0 {
            return Err(last_error.unwrap_or_else(|| invalid("translation_all_failed")));
        }
        let failed = segments.len() as i64 - translated;
        let pages: Vec<i64> = segments.iter().filter_map(|s| s.page_number).collect();
        let coverage = TranslationCoverageV1 {
            complete: failed == 0,
            total_segments: segments.len() as i64,
            translated_segments: translated,
            failed_segments: failed,
            pages: unique_sorted(pages),
            notes: if failed > 0 {
                vec![format!("{failed} 段未完成翻译,已保留原因;可重试或缩小范围")]
            } else {
                Vec::new()
            },
        };
        let coverage_report = CoverageReport {
            complete: coverage.complete,
            regions: vec![CoverageRegion {
                label: snapshot.summary.name.clone(),
                block_count: coverage.total_segments,
            }],
        };
        let content = TranslationV1 {
            schema_version: TRANSLATION_SCHEMA_VERSION.into(),
            source_language: detect_language(
                &segments
                    .iter()
                    .map(|s| s.source_text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .to_string(),
            target_language: target_language.to_string(),
            style,
            segments,
            glossary,
            coverage,
        };
        let content_json =
            serde_json::to_value(&content).map_err(|e| AppError::internal(e.to_string()))?;
        let title = title
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("翻译 · {}", snapshot.summary.name));
        self.artifacts.commit_ai_draft(
            &AiDraftCommitInput {
                document_id: document_id.to_string(),
                kind: "translation".into(),
                title,
                content: content_json,
                coverage: coverage_report,
                snapshot_id: Some(snapshot.id.clone()),
                task_id: task_id.map(str::to_string),
                citations: Vec::new(),
            },
            &format!("translation:{}", sha256_str(&snapshot.id)),
        )
    }

    fn snapshot_blocks(
        &self,
        snapshot: &ContextSnapshotView,
    ) -> Result<Vec<ContentBlockView>, AppError> {
        let mut blocks = if snapshot.block_ids.is_empty() {
            match snapshot.parse_generation_id.as_deref() {
                Some(generation_id) => self.parse.list_generation_blocks(generation_id)?,
                None => Vec::new(),
            }
        } else {
            self.parse.get_blocks_by_ids(&snapshot.block_ids)?
        };
        blocks.sort_by_key(|block| block.ordinal.unwrap_or(i64::MAX));
        blocks.retain(|block| !block.text.trim().is_empty());
        if blocks.len() > 400 {
            blocks.truncate(400);
        }
        Ok(blocks)
    }

    fn memory_get(
        &self,
        source_hash: &str,
        target: &str,
        model_id: &str,
        glossary_version: &str,
    ) -> Result<Option<String>, AppError> {
        let key = (
            source_hash.to_string(),
            target.to_string(),
            model_id.to_string(),
            glossary_version.to_string(),
        );
        self.db.read(move |conn| {
            conn.query_row(
                "SELECT translated_text FROM translation_memory
                 WHERE source_hash=?1 AND target_language=?2 AND model_id=?3
                   AND glossary_version=?4",
                params![key.0, key.1, key.2, key.3],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| db_app_error(&e))
        })
    }

    fn memory_put(
        &self,
        source_hash: &str,
        target: &str,
        model_id: &str,
        glossary_version: &str,
        translated: &str,
    ) -> Result<(), AppError> {
        if translated.trim().is_empty() {
            return Ok(());
        }
        let id = self.ids.new_entity_id();
        let now = self.clock.now_ms();
        let key = (
            source_hash.to_string(),
            target.to_string(),
            model_id.to_string(),
            glossary_version.to_string(),
            translated.to_string(),
        );
        self.db.write(move |conn| {
            conn.execute(
                "INSERT INTO translation_memory
                    (id, source_hash, target_language, model_id, glossary_version,
                     translated_text, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(source_hash, target_language, model_id, glossary_version)
                 DO UPDATE SET translated_text=excluded.translated_text",
                params![id, key.0, key.1, key.2, key.3, key.4, now],
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
    }

    pub fn clear_memory_for_document(&self, _document_id: &str) -> Result<(), AppError> {
        // 记忆按源文本 hash 共享,不按文档删除;隐私清除由 M08 贡献器按类别执行。
        Ok(())
    }
}

fn invalid(reason: &str) -> AppError {
    AppError::new(CoreErrorCode::InvalidRequest, "translation").with_detail("reason", reason.into())
}

/// 用户取消:与生成任务一致落为 TASK_CANCELED(不是失败)。
fn cancellation_error() -> AppError {
    AppError::new(CoreErrorCode::TaskCanceled, crate::shared::new_trace_id())
}

fn normalize_glossary(entries: &[GlossaryEntryV1]) -> Vec<GlossaryEntryV1> {
    let mut out = entries
        .iter()
        .filter(|entry| !entry.source.trim().is_empty() && !entry.target.trim().is_empty())
        .map(|entry| GlossaryEntryV1 {
            source: entry.source.trim().to_string(),
            target: entry.target.trim().to_string(),
        })
        .collect::<Vec<_>>();
    out.sort_by(|a, b| a.source.cmp(&b.source));
    out.dedup_by(|a, b| a.source == b.source && a.target == b.target);
    out
}

fn unique_sorted(mut pages: Vec<i64>) -> Vec<i64> {
    pages.sort_unstable();
    pages.dedup();
    pages
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
    if cjk > latin {
        "zh"
    } else {
        "en"
    }
}

/// 占位符保护:代码/公式/URL/邮箱/数字单位不翻译。
/// 纯数字/表格数值/符号无需模型翻译,原样保留,避免制造“失败段”。
fn needs_translation(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

/// 未完成的段落保留原文:用户至少能继续双语阅读,不出现空译文。
fn keep_source(segment: &mut TranslationSegmentV1, reason: &str) {
    segment.translated_text = segment.source_text.clone();
    segment.status = TranslationStatus::Failed;
    segment.confidence = TranslationConfidence::Low;
    segment.failure_reason = Some(reason.to_string());
}

pub fn protect(text: &str) -> (String, Vec<(String, String)>) {
    let mut masked = String::new();
    let mut tokens = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    let push_token = |masked: &mut String, tokens: &mut Vec<(String, String)>, value: String| {
        let token = format!("⟦P{}⟧", tokens.len());
        tokens.push((token.clone(), value));
        masked.push_str(&token);
    };
    while index < chars.len() {
        // fenced code
        if chars[index..].starts_with(&['`', '`', '`']) {
            let mut end = index + 3;
            while end + 2 < chars.len()
                && !(chars[end] == '`' && chars[end + 1] == '`' && chars[end + 2] == '`')
            {
                end += 1;
            }
            let end = (end + 3).min(chars.len());
            push_token(&mut masked, &mut tokens, chars[index..end].iter().collect());
            index = end;
            continue;
        }
        if chars[index] == '`' {
            let mut end = index + 1;
            while end < chars.len() && chars[end] != '`' {
                end += 1;
            }
            let end = (end + 1).min(chars.len());
            push_token(&mut masked, &mut tokens, chars[index..end].iter().collect());
            index = end;
            continue;
        }
        // inline math
        if chars[index] == '$' {
            let mut end = index + 1;
            while end < chars.len() && chars[end] != '$' {
                end += 1;
            }
            if end < chars.len() {
                end += 1;
                push_token(&mut masked, &mut tokens, chars[index..end].iter().collect());
                index = end;
                continue;
            }
        }
        // URL
        let rest: String = chars[index..].iter().take(8).collect();
        if rest.starts_with("http://") || rest.starts_with("https://") {
            let mut end = index;
            while end < chars.len() && !chars[end].is_whitespace() {
                end += 1;
            }
            push_token(&mut masked, &mut tokens, chars[index..end].iter().collect());
            index = end;
            continue;
        }
        // email / number+unit: consume a non-space token and decide
        if !chars[index].is_whitespace() {
            let mut end = index;
            while end < chars.len() && !chars[end].is_whitespace() {
                end += 1;
            }
            let token: String = chars[index..end].iter().collect();
            let trimmed = token
                .trim_matches(|c: char| matches!(c, ',' | ';' | ')' | ']' | '。' | '，' | '；'));
            let is_email = trimmed.contains('@') && trimmed.contains('.');
            let is_number = trimmed.chars().next().is_some_and(|c| c.is_ascii_digit())
                && trimmed.chars().any(|c| c.is_ascii_digit())
                && trimmed.chars().all(|c| {
                    c.is_ascii_digit()
                        || matches!(c, '.' | ',' | '%' | '/' | '-' | '_' | '+')
                        || c.is_alphabetic()
                });
            if is_email || is_number {
                push_token(&mut masked, &mut tokens, token);
                index = end;
                continue;
            }
        }
        masked.push(chars[index]);
        index += 1;
    }
    (masked, tokens)
}

pub fn restore(masked: &str, tokens: &[(String, String)]) -> String {
    let mut out = masked.to_string();
    for (token, original) in tokens {
        out = out.replace(token, original);
    }
    // 模型常改写占位符括号(⟦P1⟧ → ⟦P1⟫、【P1】、[P1]、P1):再做一次宽松还原,
    // 避免把内部占位符原样暴露给用户。长 token 先处理(P10 先于 P1)。
    let mut ordered: Vec<(&String, &String)> = tokens
        .iter()
        .map(|(token, original)| (token, original))
        .collect();
    ordered.sort_by_key(|(token, _)| std::cmp::Reverse(token.chars().count()));
    for (token, original) in ordered {
        let inner = token.trim_start_matches('⟦').trim_end_matches('⟧');
        if !inner.is_empty() {
            out = restore_token_variants(&out, inner, original);
        }
    }
    out
}

fn is_token_bracket(ch: char) -> bool {
    matches!(
        ch,
        '⟦' | '⟧'
            | '⟪'
            | '⟫'
            | '【'
            | '】'
            | '〖'
            | '〗'
            | '〔'
            | '〕'
            | '〚'
            | '〛'
            | '（'
            | '）'
            | '['
            | ']'
            | '('
            | ')'
    ) || ('\u{2190}'..='\u{2BFF}').contains(&ch)
}

fn restore_token_variants(text: &str, inner: &str, original: &str) -> String {
    if !text.contains(inner) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let inner_chars: Vec<char> = inner.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index..].starts_with(&inner_chars) {
            let mut end = index + inner_chars.len();
            // 更长的 token(P10)不能被 P1 前缀误匹配。
            if end < chars.len() && chars[end].is_ascii_digit() {
                out.push(chars[index]);
                index += 1;
                continue;
            }
            let mut start = index;
            if start >= 1 && is_token_bracket(chars[start - 1]) {
                start -= 1;
            } else if start >= 2 && chars[start - 1] == ' ' && is_token_bracket(chars[start - 2]) {
                start -= 2;
            }
            if end < chars.len() && is_token_bracket(chars[end]) {
                end += 1;
            } else if end + 1 < chars.len() && chars[end] == ' ' && is_token_bracket(chars[end + 1])
            {
                end += 2;
            }
            // 左侧括号/空格已在扫描中输出:按数量回退,不依赖 out 的绝对长度
            // (前面的占位符替换可能让 out 比 chars 短,不能用总长度比较)。
            for _ in 0..(index - start) {
                out.pop();
            }
            out.push_str(original);
            index = end;
            continue;
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

fn tokens_intact(translated: &str, tokens: &[(String, String)]) -> bool {
    // canonical token 或它对应的原文值任一存在,都说明占位内容被保留
    // (restore 会把括号变体还原成原文值)。
    tokens
        .iter()
        .all(|(token, original)| translated.contains(token) || translated.contains(original))
}

fn status_of(confidence: TranslationConfidence) -> TranslationStatus {
    match confidence {
        TranslationConfidence::Low => TranslationStatus::LowConfidence,
        _ => TranslationStatus::Translated,
    }
}

fn confidence_of(
    source: &str,
    translated: &str,
    tokens: &[(String, String)],
    glossary: &[GlossaryEntryV1],
) -> TranslationConfidence {
    if translated.trim().is_empty() {
        return TranslationConfidence::Low;
    }
    let intact = tokens_intact(translated, tokens);
    let ratio = translated.chars().count() as f64 / source.chars().count().max(1) as f64;
    let glossary_ok = glossary
        .iter()
        .all(|entry| !source.contains(&entry.source) || translated.contains(&entry.target));
    if intact && glossary_ok && (0.4..=3.0).contains(&ratio) {
        TranslationConfidence::High
    } else if intact && (0.25..=4.0).contains(&ratio) {
        TranslationConfidence::Medium
    } else {
        TranslationConfidence::Low
    }
}

fn glossary_hits(source: &str, glossary: &[GlossaryEntryV1]) -> Vec<String> {
    glossary
        .iter()
        .filter(|entry| source.contains(&entry.source))
        .map(|entry| format!("{}→{}", entry.source, entry.target))
        .collect()
}

fn parse_translations(value: &serde_json::Value) -> Result<HashMap<String, String>, AppError> {
    let mut map = HashMap::new();
    // 兼容模型两种常见形状:数组 [{id,text}] 或对象 {"i1":"译文"}。
    match value.get("translations") {
        Some(serde_json::Value::Array(array)) => {
            for item in array {
                let Some(id) = item.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let text = ["text", "translated", "translatedText", "translation"]
                    .iter()
                    .find_map(|key| item.get(*key).and_then(|v| v.as_str()));
                let Some(text) = text else {
                    continue;
                };
                insert_translation(&mut map, id, text);
            }
        }
        Some(serde_json::Value::Object(entries)) => {
            for (id, entry) in entries {
                let text = entry.as_str().or_else(|| {
                    ["text", "translated", "translatedText", "translation"]
                        .iter()
                        .find_map(|key| entry.get(*key).and_then(|v| v.as_str()))
                });
                if let Some(text) = text {
                    insert_translation(&mut map, id, text);
                }
            }
        }
        _ => {
            return Err(invalid("translation_model_output_invalid"));
        }
    }
    if map.is_empty() {
        return Err(invalid("translation_model_output_empty"));
    }
    Ok(map)
}

/// 同时记录原文 id 与“尾随数字”别名:模型常把 "i1" 写成 "1"、"I1"、
/// "t1" 或带引号的变体;数字后缀能稳定对应回请求里的段。
fn insert_translation(map: &mut HashMap<String, String>, id: &str, text: &str) {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return;
    }
    map.insert(trimmed.to_string(), text.to_string());
    let lower = trimmed.to_lowercase();
    if lower != trimmed {
        map.insert(lower, text.to_string());
    }
    if let Some(digits) = trailing_digits(trimmed) {
        map.entry(digits.to_string())
            .or_insert_with(|| text.to_string());
    }
}

fn trailing_digits(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    let mut end = bytes.len();
    while end > 0 && !bytes[end - 1].is_ascii_digit() {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut start = end;
    while start > 0 && bytes[start - 1].is_ascii_digit() {
        start -= 1;
    }
    Some(&value[start..end])
}

/// 按请求 id 查找译文:先精确匹配,再按尾随数字别名匹配。
pub fn lookup_translation<'a>(map: &'a HashMap<String, String>, id: &str) -> Option<&'a String> {
    map.get(id)
        .or_else(|| trailing_digits(id).and_then(|digits| map.get(digits)))
}

fn translation_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["schemaVersion", "translations"],
        "properties": {
            "schemaVersion": {"const": TRANSLATION_SCHEMA_VERSION},
            "translations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "text"],
                    "properties": {
                        "id": {"type": "string"},
                        "text": {"type": "string"}
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instruction_echo_is_rejected_as_translation() {
        assert!(looks_like_instruction(
            "将每个 SOURCE 片段翻译为 zh-CN。返回 JSON:{\"translations\":[...]} schemaVersion=translation.v1"
        ));
        assert!(looks_like_instruction(
            "respond with {\"translations\":[{\"id\":\"SOURCE id\",\"text\":\"translated text\"}]}"
        ));
        // 中文指令回显同样拒绝(实测弱模型会返回整段中文指令)。
        assert!(looks_like_instruction(
            "翻译测试件\n将每个 SOURCE 片段翻译为简体中文(zh-CN),风格忠实。每个占位符标记须原样保留。"
        ));
        assert!(!looks_like_instruction("本地优先的阅读工作区。"));
    }

    #[test]
    fn unchanged_sentence_is_not_a_translation() {
        assert!(translation_not_applied(
            "The quick brown fox jumps over the lazy dog.",
            "The quick brown fox jumps over the lazy dog."
        ));
        // 短专名/同一语言短词不误判。
        assert!(!translation_not_applied("Knowwise", "Knowwise"));
        assert!(!translation_not_applied(
            "The quick brown fox jumps over the lazy dog.",
            "敏捷的棕色狐狸跃过懒狗。"
        ));
    }

    #[test]
    fn translation_ids_tolerate_numeric_case_and_object_shapes() {
        let array_shape = serde_json::json!({
            "translations": [
                {"id": "1", "text": "译文一"},
                {"id": "I2", "translatedText": "译文二"}
            ]
        });
        let map = parse_translations(&array_shape).unwrap();
        assert_eq!(
            lookup_translation(&map, "i1").map(String::as_str),
            Some("译文一")
        );
        assert_eq!(
            lookup_translation(&map, "i2").map(String::as_str),
            Some("译文二")
        );
        let object_shape = serde_json::json!({
            "translations": {"t1": "译文A", "t2": {"text": "译文B"}}
        });
        let map = parse_translations(&object_shape).unwrap();
        assert_eq!(
            lookup_translation(&map, "t1").map(String::as_str),
            Some("译文A")
        );
        assert_eq!(
            lookup_translation(&map, "t2").map(String::as_str),
            Some("译文B")
        );
        // 完全无法识别的 id 仍如实报缺失,不猜配。
        assert!(lookup_translation(&map, "t9").is_none());
    }

    #[test]
    fn placeholders_protect_code_urls_math_and_numbers() {
        let (masked, tokens) = protect("见 https://a.b/c 和 `code` 以及 $x^2$，预算 120 万元。");
        assert!(masked.contains("⟦P0⟧"));
        assert!(!masked.contains("https://"));
        assert!(!masked.contains("`code`"));
        assert_eq!(
            restore(&masked, &tokens),
            "见 https://a.b/c 和 `code` 以及 $x^2$，预算 120 万元。"
        );
    }

    #[test]
    fn placeholder_bracket_variants_are_restored() {
        let tokens = vec![
            ("⟦P0⟧".to_string(), "1".to_string()),
            ("⟦P1⟧".to_string(), "https://a.b/c".to_string()),
        ];
        assert_eq!(restore("第 ⟦P0⟧ 页", &tokens), "第 1 页");
        // 模型实测把右括号写成 ⟫(U+27EB)。
        assert_eq!(restore("第 ⟦P0⟫ 页", &tokens), "第 1 页");
        // 也观察到自造箭头/数学符号作为右括号(⟺,U+27FA)。
        assert_eq!(restore("第 ⟦P0⟺ 页", &tokens), "第 1 页");
        assert_eq!(restore("第 【P0】 页", &tokens), "第 1 页");
        assert_eq!(restore("第 P0 页", &tokens), "第 1 页");
        assert_eq!(restore("见 ⟪P1⟫", &tokens), "见 https://a.b/c");
        // P1 不能误匹配 P10。
        let tokens = vec![
            ("⟦P1⟧".to_string(), "甲".to_string()),
            ("⟦P10⟧".to_string(), "乙".to_string()),
        ];
        assert_eq!(restore("⟦P10⟧ 与 【P1】", &tokens), "乙 与 甲");
    }

    #[test]
    fn numeric_and_symbolic_segments_skip_model_translation() {
        assert!(!needs_translation("8.00\n8.00\n9.00"));
        assert!(!needs_translation("→：，，，，"));
        assert!(needs_translation("预算阈值"));
        assert!(needs_translation("Chapter 12"));
    }

    #[test]
    fn already_cancelled_translation_never_calls_the_model() {
        let path = std::env::temp_dir().join(format!(
            "kw-translation-{}.sqlite3",
            crate::shared::SystemIdGenerator.new_entity_id()
        ));
        let db = Arc::new(Db::open(&path, 1).unwrap());
        db.write(|conn| {
            let migrations = crate::persistence::registration::migrations()
                .into_iter()
                .chain(crate::document::registration::migrations())
                .chain(crate::parse_index::registration::migrations())
                .chain(crate::artifacts::registration::migrations())
                .chain(crate::translation::registration::migrations());
            for migration in migrations {
                conn.execute_batch(migration.sql)
                    .map_err(|e| db_app_error(&e))?;
            }
            conn.execute_batch(
                "INSERT INTO document(id,display_name,format,current_version_id,created_at,updated_at)
                   VALUES ('d1','test','pdf','v1',0,0);
                 INSERT INTO document_version(id,document_id,fingerprint_status,created_at)
                   VALUES ('v1','d1','ready',0);",
            )
            .map_err(|e| db_app_error(&e))?;
            Ok(())
        })
        .unwrap();
        let clock = Arc::new(crate::shared::SystemClock);
        let ids = Arc::new(crate::shared::SystemIdGenerator);
        let events = Arc::new(crate::shared::NoopEventSink);
        let tasks = Arc::new(crate::persistence::task::TaskService::new(
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
            tasks,
        ));
        let artifacts = Arc::new(ArtifactService::new(
            db.clone(),
            clock.clone(),
            ids.clone(),
            events.clone(),
        ));
        let generation = Arc::new(CountingGeneration::default());
        let service =
            TranslationService::new(db.clone(), clock, ids, parse, generation.clone(), artifacts);
        let block = service
            .parse
            .ensure_selection_blocks(&crate::parse_index::models::SelectionBlockRequest {
                document_id: "d1".into(),
                version_id: "v1".into(),
                text: "本地优先的文档工作台需要可追溯的引用。".into(),
                locator: crate::shared::SourceLocator::pdf(
                    1,
                    vec![],
                    crate::shared::locator::PageRotation::R0,
                ),
                heading_path: vec![],
            })
            .unwrap();
        let snapshot = translation_snapshot(block.id);
        let cancel = tokio_util::sync::CancellationToken::new();
        cancel.cancel();
        let error = service
            .translate_with_cancel("d1", &snapshot, "zh-CN", None, &[], None, None, &cancel)
            .unwrap_err();
        assert_eq!(error.code, "TASK_CANCELED");
        assert_eq!(
            generation.calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "取消后不得再发起模型调用"
        );
        // 取消不落产物:同快照不会留下 translation 草稿。
        let count: i64 = db
            .read(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM artifact WHERE kind='translation'",
                    [],
                    |r| r.get(0),
                )
                .map_err(|e| db_app_error(&e))
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[derive(Default)]
    struct CountingGeneration {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl StructuredGenerationPort for CountingGeneration {
        fn capability(
            &self,
            _: &str,
        ) -> Result<crate::retrieval_ai::ports::ModelCapabilityView, AppError> {
            Ok(crate::retrieval_ai::ports::ModelCapabilityView {
                generation_available: true,
                generation_authorized: true,
                provider_id: Some("test-provider".into()),
                model_id: Some("test-model".into()),
            })
        }

        fn generate(
            &self,
            _: &StructuredGenerationRequest,
            _: &tokio_util::sync::CancellationToken,
        ) -> Result<crate::retrieval_ai::models::StructuredGenerationResult, AppError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(crate::retrieval_ai::models::StructuredGenerationResult {
                content: serde_json::json!({
                    "schemaVersion": TRANSLATION_SCHEMA_VERSION,
                    "translations": []
                }),
                model_provider_id: "test-provider".into(),
                model_id: "test-model".into(),
                partial: false,
            })
        }
    }

    fn translation_snapshot(block_id: String) -> ContextSnapshotView {
        ContextSnapshotView {
            id: "snap".into(),
            document_id: "d1".into(),
            document_version_id: "v1".into(),
            parse_generation_id: None,
            scope_kind: crate::context::models::ScopeKind::Selection,
            scope: crate::context::models::ScopeDraft::Section {
                heading_path: vec![],
                block_ids: Some(vec![block_id.clone()]),
            },
            summary: crate::context::models::SnapshotSummary {
                name: "选择范围".into(),
                block_count: 1,
                source_count: 1,
                char_count: 20,
                token_estimate: 10,
            },
            coverage: crate::context::models::CoverageSummary::empty("complete"),
            block_ids: vec![block_id],
            block_set_hash: None,
            canonical_hash: "fixture".into(),
            sources: vec![],
            created_at: 1,
        }
    }
}

/// 2G 隐私清除:翻译记忆为跨文档缓存,清除类别会清空全部记忆条目。
pub struct TranslationMemoryClearContributor {
    db: Arc<Db>,
}

impl TranslationMemoryClearContributor {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

impl crate::model_privacy::clear::DataClearContributor for TranslationMemoryClearContributor {
    fn contributor_id(&self) -> &'static str {
        "m13_translation_memory"
    }
    fn supported_categories(&self) -> &'static [crate::model_privacy::models::ClearCategory] {
        &[crate::model_privacy::models::ClearCategory::TranslationMemory]
    }
    fn preview(
        &self,
        _document_id: &str,
        categories: &[crate::model_privacy::models::ClearCategory],
    ) -> Result<Vec<crate::model_privacy::models::ClearCategoryPreview>, AppError> {
        use crate::model_privacy::models::{ClearCategory, ClearCategoryPreview};
        if !categories.contains(&ClearCategory::TranslationMemory) {
            return Ok(Vec::new());
        }
        let count: i64 = self.db.read(|conn| {
            conn.query_row("SELECT COUNT(*) FROM translation_memory", [], |row| {
                row.get(0)
            })
            .map_err(|e| db_app_error(&e))
        })?;
        Ok(vec![ClearCategoryPreview {
            category: ClearCategory::TranslationMemory,
            available: true,
            unavailable_reason: None,
            count: Some(count),
            affected_citations: Some(0),
            protected_count: None,
        }])
    }
    fn execute(
        &self,
        _document_id: &str,
        categories: &[crate::model_privacy::models::ClearCategory],
    ) -> Result<Vec<crate::model_privacy::models::ClearCategoryExecution>, AppError> {
        use crate::model_privacy::models::{ClearCategory, ClearCategoryExecution};
        if !categories.contains(&ClearCategory::TranslationMemory) {
            return Ok(Vec::new());
        }
        let deleted: i64 = self.db.write(|conn| {
            conn.execute("DELETE FROM translation_memory", [])
                .map_err(|e| db_app_error(&e))
                .map(|n| n as i64)
        })?;
        Ok(vec![ClearCategoryExecution {
            category: ClearCategory::TranslationMemory,
            deleted,
        }])
    }
}

/// 内联翻译共用的 JSON schema(与整篇翻译同形,供阅读器就地翻译使用)。
pub fn inline_translation_schema() -> serde_json::Value {
    translation_schema()
}

/// 内联翻译响应解析:只取 id → 文本,缺项由调用方标记失败。
pub fn parse_inline_translations(value: &serde_json::Value) -> HashMap<String, String> {
    parse_translations(value).unwrap_or_default()
}

/// 模型把"翻译指令"当原文翻译回来(实测发生,中英文都可能):这些片段不是译文,
/// 必须整段拒绝,不能当成结果展示。
pub fn looks_like_instruction(text: &str) -> bool {
    let lower = text.to_lowercase();
    const MARKERS: [&str; 13] = [
        "schemaversion=translation.v1",
        "source id",
        "translate each source",
        "translations\":[",
        "⟦p0⟧ exactly",
        "translated text",
        "respond with json",
        "将每个 source 片段翻译为",
        "每个占位符标记",
        "不要翻译代码",
        "风格忠实",
        "当源文出现以下术语",
        "应用本术语表",
    ];
    MARKERS.iter().any(|marker| lower.contains(marker))
}

/// 译文与原文完全一致(且是成句文本)时视为"没有真正翻译"。
pub fn translation_not_applied(source: &str, translated: &str) -> bool {
    let s = source.trim();
    let t = translated.trim();
    s == t && s.chars().count() >= 20 && s.split_whitespace().count() >= 3
}
