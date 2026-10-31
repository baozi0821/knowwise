//! 离线翻译引擎(不经过模型、不联网)。
//!
//! 设计目标与浏览器内置翻译一致:本地跑一个**专用翻译模型**(OPUS-MT /
//! Marian 系列的 seq2seq),纯 CPU 推理,不调用大语言模型、不发送正文。
//!
//! 分层:
//! - `TranslationEngine` 是端口:上层只依赖它,不关心具体推理后端;
//! - `OnnxMarianEngine` 是 ONNX Runtime 实现(句级编码→解码,带 beam 搜索);
//! - 引擎不可用(模型未安装/后端未编译)时明确返回 `EngineUnavailable`,
//!   由调用方显示"本地翻译模型未安装"并提供安装说明,**绝不**悄悄回退到
//!   远程模型冒充本地翻译。
//!
//! 模型文件存放于应用数据目录 `models/translate/<方向>/`,首次安装由用户
//! 或安装器放入;应用只读,不自行下载(离线优先,不在后台偷偷联网)。

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::shared::AppError;

/// 一次翻译请求里的单句(已按句末标点切分)。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSentence {
    pub text: String,
}

/// 译文与质量信息。
#[derive(Debug, Clone, PartialEq)]
pub struct TranslatedSentence {
    pub source: String,
    pub text: String,
    /// 0..1;低置信表示模型对这句把握不大(仍可用,由 UI 标注)。
    pub confidence: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineStatus {
    /// 模型已安装且后端可用。
    Ready,
    /// 该语言方向没有安装模型。
    ModelMissing,
    /// 后端本身不可用(例如构建时未启用 ONNX 特性)。
    BackendUnavailable,
}

#[derive(Debug, Clone)]
pub struct EngineInfo {
    pub status: EngineStatus,
    pub engine_name: String,
    pub source_language: String,
    pub target_language: String,
    /// 模型目录(便于用户核对是否已放置模型;不含密钥)。
    pub model_dir: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum TranslationEngineError {
    Unavailable(EngineStatus),
    /// 句子过长,超出模型单次处理的 token 上限。
    SentenceTooLong {
        chars: usize,
        limit: usize,
    },
    /// 推理失败(模型损坏/显存等),附稳定错误码。
    InferenceFailed(String),
}

impl From<TranslationEngineError> for AppError {
    fn from(error: TranslationEngineError) -> Self {
        match error {
            TranslationEngineError::Unavailable(status) => {
                let reason = match status {
                    EngineStatus::ModelMissing => "local_translation_model_missing",
                    EngineStatus::BackendUnavailable => "local_translation_backend_unavailable",
                    EngineStatus::Ready => "local_translation_unavailable",
                };
                AppError::new(
                    crate::shared::CoreErrorCode::InvalidRequest,
                    crate::shared::new_trace_id(),
                )
                .with_detail("reason", reason.into())
            }
            TranslationEngineError::SentenceTooLong { chars, limit } => AppError::new(
                crate::shared::CoreErrorCode::InvalidRequest,
                crate::shared::new_trace_id(),
            )
            .with_detail("reason", "sentence_too_long".into())
            .with_detail("chars", (chars as i64).into())
            .with_detail("limit", (limit as i64).into()),
            TranslationEngineError::InferenceFailed(code) => {
                AppError::internal(format!("local_translation_failed:{code}"))
            }
        }
    }
}

/// 离线翻译端口。实现必须**纯本地**:不得发起任何网络请求。
pub trait TranslationEngine: Send + Sync {
    fn info(&self, source: &str, target: &str) -> EngineInfo;

    /// 批量翻译。保持输入顺序;空串原样返回空串。
    fn translate_batch(
        &self,
        source_language: &str,
        target_language: &str,
        sentences: &[String],
    ) -> Result<Vec<TranslatedSentence>, TranslationEngineError>;
}

/// Sentence limit: OPUS-MT is trained on 512-token segments. 400 characters is
/// a conservative limit for CJK text, where one character is often one token.
pub const MAX_SENTENCE_CHARS: usize = 400;

/// 把一个段落按句末标点切成句子,供模型逐句翻译。
///
/// 只做"切分",不改写内容:切分点之后的内容原样保留,拼接后与原文等长
/// (忽略标点归属差异)。过长的句子按逗号/空格二次切分,避免超长失配。
pub fn split_sentences(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        current.push(*ch);
        let next_is_space = chars
            .get(index + 1)
            .map(|next| next.is_whitespace())
            .unwrap_or(true);
        let boundary = matches!(ch, '。' | '！' | '？' | '；' | '!' | '?' | ';')
            || (*ch == '.' && next_is_space && looks_like_sentence_dot(&current));
        if boundary {
            out.push(std::mem::take(&mut current));
        }
        if current.chars().count() >= MAX_SENTENCE_CHARS {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.trim().is_empty() {
        out.push(current);
    }
    out.into_iter()
        .filter(|sentence| !sentence.trim().is_empty())
        .flat_map(|sentence| split_long_sentence(&sentence))
        .collect()
}

/// 英文句点只在"后面是空白且前面不是数字"时视作句末,避免切开 3.14 / v1.2。
fn looks_like_sentence_dot(buffer: &str) -> bool {
    // buffer 以正在处理的 '.' 结尾;看它**前一个**字符:
    // 数字(3.14)或单字母缩写不算句末,其余(单词结束)算。
    let mut chars = buffer.chars().rev();
    chars.next(); // 跳过 '.'
    let Some(previous) = chars.next() else {
        return false;
    };
    !previous.is_ascii_digit()
}

fn split_long_sentence(sentence: &str) -> Vec<String> {
    let total = sentence.chars().count();
    if total <= MAX_SENTENCE_CHARS {
        return vec![sentence.to_string()];
    }
    // 把长句切成接近上限的多段:在软切分点(逗号/顿号/分号/空格)处断开,
    // 避免把词切在中间;找不到软切分点时按上限硬切,保证不超长。
    let target = MAX_SENTENCE_CHARS;
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut last_soft_break: Option<usize> = None;
    for ch in sentence.chars() {
        current.push(ch);
        if matches!(ch, '，' | ',' | '、' | '：' | ':' | ';' | '；' | ' ') {
            last_soft_break = Some(current.chars().count());
        }
        if current.chars().count() >= target {
            let cut = last_soft_break.unwrap_or(current.chars().count());
            let chars: Vec<char> = current.chars().collect();
            let head: String = chars[..cut].iter().collect();
            let tail: String = chars[cut..].iter().collect();
            if !head.trim().is_empty() {
                parts.push(head);
            }
            current = tail;
            last_soft_break = None;
        }
    }
    if !current.trim().is_empty() {
        parts.push(current);
    }
    parts
}

type Engine = Box<dyn TranslationEngine>;
type EngineSlot = Mutex<Option<Engine>>;
type EngineGuard<'a> = std::sync::MutexGuard<'a, Option<Engine>>;
type EngineFactory = Box<dyn Fn(&Path) -> Option<Engine> + Send + Sync>;

/// 应用内的引擎持有者:惰性初始化 + 线程安全。
pub struct LocalTranslator {
    engine: EngineSlot,
    factory: EngineFactory,
    models_root: PathBuf,
}

impl LocalTranslator {
    pub fn new(
        models_root: PathBuf,
        factory: impl Fn(&Path) -> Option<Engine> + Send + Sync + 'static,
    ) -> Self {
        Self {
            engine: Mutex::new(None),
            factory: Box::new(factory),
            models_root,
        }
    }

    /// 仅探测状态,不加载模型(用于设置页/状态栏)。
    pub fn info(&self, source: &str, target: &str) -> EngineInfo {
        let dir = self.model_dir(source, target);
        let installed = dir.is_dir();
        let ready = installed && backend_probe(&dir);
        let guard = self.engine.lock().unwrap();
        match guard.as_ref() {
            Some(engine) => engine.info(source, target),
            None => EngineInfo {
                status: if ready {
                    EngineStatus::Ready
                } else if installed {
                    EngineStatus::BackendUnavailable
                } else {
                    EngineStatus::ModelMissing
                },
                engine_name: "opus-mt (onnx)".into(),
                source_language: source.into(),
                target_language: target.into(),
                model_dir: Some(dir),
            },
        }
    }

    pub fn model_dir(&self, source: &str, target: &str) -> PathBuf {
        let target = normalize_lang(target);
        if is_auto(source) {
            if let Some(dir) = self.find_model_dir_for_target(&target) {
                return dir;
            }
        }
        self.models_root
            .join(format!("{}-{}", normalize_lang(source), target))
    }

    fn find_model_dir_for_target(&self, target: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.models_root).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = path.file_name()?.to_str()?;
            if let Some((_, dir_target)) = name.split_once('-') {
                if dir_target == target {
                    return Some(path);
                }
            }
        }
        None
    }

    /// 取引擎;模型目录不存在时直接返回 ModelMissing,不尝试加载。
    fn engine_for(
        &self,
        source: &str,
        target: &str,
    ) -> Result<EngineGuard<'_>, TranslationEngineError> {
        let dir = self.model_dir(source, target);
        if !dir.is_dir() {
            return Err(TranslationEngineError::Unavailable(
                EngineStatus::ModelMissing,
            ));
        }
        let mut guard = self.engine.lock().unwrap();
        if guard.is_none() {
            match (self.factory)(&dir) {
                Some(engine) => *guard = Some(engine),
                None => {
                    // 工厂返回 None = 后端未编译进来(或模型文件损坏)。
                    return Err(TranslationEngineError::Unavailable(
                        EngineStatus::BackendUnavailable,
                    ));
                }
            }
        }
        Ok(guard)
    }

    pub fn translate_batch(
        &self,
        source: &str,
        target: &str,
        sentences: &[String],
    ) -> Result<Vec<TranslatedSentence>, TranslationEngineError> {
        for sentence in sentences {
            let chars = sentence.chars().count();
            if chars > MAX_SENTENCE_CHARS {
                return Err(TranslationEngineError::SentenceTooLong {
                    chars,
                    limit: MAX_SENTENCE_CHARS,
                });
            }
        }
        // Inline translation does not know the source language; infer it from
        // the text so `zh-en` / `en-zh` are selected instead of a nonexistent
        // `auto-*` directory.
        let source = resolve_source_language(source, sentences);
        let guard = self.engine_for(source, target)?;
        let engine = guard.as_ref().ok_or(TranslationEngineError::Unavailable(
            EngineStatus::BackendUnavailable,
        ))?;
        engine.translate_batch(source, target, sentences)
    }
}

fn is_auto(source: &str) -> bool {
    matches!(source.trim().to_ascii_lowercase().as_str(), "auto" | "")
}

fn backend_probe(dir: &Path) -> bool {
    #[cfg(feature = "local-mt")]
    {
        crate::translation::onnx_mt::probe(dir)
    }
    #[cfg(not(feature = "local-mt"))]
    {
        let _ = dir;
        false
    }
}

fn resolve_source_language<'a>(source: &'a str, sentences: &'a [String]) -> &'a str {
    if !is_auto(source) {
        return source;
    }
    sentences
        .iter()
        .find_map(|sentence| {
            let text = sentence.trim();
            if text.is_empty() {
                None
            } else {
                Some(detect_source_language(text))
            }
        })
        .unwrap_or("en")
}

/// Small script-based detector for the languages with local models. It is not
/// a general language identifier: unknown scripts fall back to English and
/// will report `ModelMissing` if no matching model is installed.
pub fn detect_source_language(text: &str) -> &'static str {
    let mut has_cjk = false;
    for ch in text.chars() {
        let code = ch as u32;
        if (0x3040..=0x30FF).contains(&code) {
            return "ja";
        }
        if (0xAC00..=0xD7AF).contains(&code) {
            return "ko";
        }
        if (0x3400..=0x4DBF).contains(&code)
            || (0x4E00..=0x9FFF).contains(&code)
            || (0xF900..=0xFAFF).contains(&code)
        {
            has_cjk = true;
        }
    }
    if has_cjk {
        "zh"
    } else {
        "en"
    }
}

/// BCP-47 / 常见名称 → OPUS-MT 目录名用的短代码。
pub fn normalize_lang(language: &str) -> String {
    let lowered = language.trim().to_lowercase();
    let primary = lowered.split(['-', '_']).next().unwrap_or("zh");
    match primary {
        "zh" | "cmn" => "zh".into(),
        "en" => "en".into(),
        "ja" => "ja".into(),
        "ko" => "ko".into(),
        "de" => "de".into(),
        "fr" => "fr".into(),
        "es" => "es".into(),
        "ru" => "ru".into(),
        "pt" => "pt".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_chinese_and_english_sentences() {
        let zh = split_sentences("快照不可变。追问可以复用同一快照！");
        assert_eq!(
            zh,
            vec!["快照不可变。", "追问可以复用同一快照！"],
            "实际切分: {zh:?}"
        );
        let en = split_sentences("Snapshots are immutable. They can be reused.");
        assert_eq!(en.len(), 2);
    }

    #[test]
    fn does_not_split_decimals_or_versions() {
        let parts = split_sentences("版本 v1.2 的阈值是 3.14 左右。");
        assert_eq!(parts.len(), 1, "数字中的点不能被当成句末: {parts:?}");
    }

    #[test]
    fn keeps_short_fragment_and_empty_input_safe() {
        assert_eq!(split_sentences(""), Vec::<String>::new());
        assert_eq!(split_sentences("好"), vec!["好"]);
    }

    #[test]
    fn long_sentence_is_split_at_commas() {
        // 真实形态:一个没有句号、但用逗号不断接续的超长句。
        let long = format!(
            "{},最终结束。",
            vec!["这是一段用于测试超长输入如何被切分的描述性文字"; 120].join(",")
        );
        let parts = split_sentences(&long);
        assert!(parts.len() > 1, "超长句必须被切分: {parts:?}");
        assert!(parts
            .iter()
            .all(|p| p.chars().count() <= MAX_SENTENCE_CHARS));
    }

    #[test]
    fn missing_model_dir_reports_model_missing() {
        let dir = std::env::temp_dir().join(format!("kw-mt-{}", uuid::Uuid::now_v7()));
        let translator = LocalTranslator::new(dir, |_| None);
        let info = translator.info("zh", "en");
        assert_eq!(info.status, EngineStatus::ModelMissing);
        let error = translator
            .translate_batch("zh", "en", &["你好".into()])
            .unwrap_err();
        assert!(matches!(
            error,
            TranslationEngineError::Unavailable(EngineStatus::ModelMissing)
        ));
    }

    #[test]
    fn normalizes_language_tags() {
        assert_eq!(normalize_lang("zh-CN"), "zh");
        assert_eq!(normalize_lang("en_US"), "en");
        assert_eq!(normalize_lang("ja"), "ja");
    }

    #[test]
    fn detects_source_script_and_resolves_auto() {
        assert_eq!(detect_source_language("你好，世界。"), "zh");
        assert_eq!(detect_source_language("Hello world."), "en");
        assert_eq!(detect_source_language("こんにちは"), "ja");
        assert_eq!(detect_source_language("안녕하세요"), "ko");
        let sentences = vec!["你好，世界。".to_string()];
        assert_eq!(resolve_source_language("auto", &sentences), "zh");
        assert_eq!(resolve_source_language("en", &sentences), "en");
    }

    #[test]
    fn auto_model_dir_matches_the_target_side() {
        let root = std::env::temp_dir().join(format!("kw-mt-auto-{}", uuid::Uuid::now_v7()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("en-zh")).unwrap();
        std::fs::create_dir_all(root.join("zh-en")).unwrap();
        let translator = LocalTranslator::new(root.clone(), |_| None);
        assert_eq!(translator.model_dir("auto", "zh-CN"), root.join("en-zh"));
        assert_eq!(translator.model_dir("auto", "en"), root.join("zh-en"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(feature = "local-mt")]
    #[test]
    fn status_is_ready_before_the_engine_is_loaded_when_files_are_present() {
        let root = std::env::temp_dir().join(format!("kw-mt-probe-{}", uuid::Uuid::now_v7()));
        let model = root.join("zh-en");
        std::fs::create_dir_all(model.join("runtime")).unwrap();
        for name in ["encoder_model.onnx", "decoder_model.onnx", "tokenizer.json"] {
            std::fs::write(model.join(name), b"").unwrap();
        }
        let library = if cfg!(target_os = "windows") {
            "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            "libonnxruntime.dylib"
        } else {
            "libonnxruntime.so"
        };
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        std::fs::write(root.join("runtime").join(library), b"").unwrap();

        let translator = LocalTranslator::new(root.clone(), |_| None);
        assert_eq!(translator.info("zh", "en").status, EngineStatus::Ready);
        std::fs::remove_dir_all(&root).ok();
    }
}
