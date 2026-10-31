//! OPUS-MT / Marian translation through ONNX Runtime.
//!
//! This backend is compiled only with the `local-mt` feature. The ONNX Runtime
//! shared library is loaded dynamically at runtime, and model weights are
//! installed by the user (`scripts/install-models.mjs`) into the application
//! data directory. No model weights or native runtime binaries are bundled in
//! the source repository.
//!
//! Expected files in `<source>-<target>/` (flat, or under `onnx/`):
//! - `encoder_model.onnx` (or `encoder_model_quantized.onnx`)
//! - `decoder_model.onnx` (or `decoder_model_quantized.onnx`; no past inputs)
//! - `tokenizer.json`
//! - `generation_config.json` / `config.json` (optional special-token ids)
//!
//! Runtime lookup order: `ORT_DYLIB_PATH`, `<models-root>/runtime/<library>`,
//! `<model-dir>/runtime/<library>`, then the model directory itself.
//!
//! The model is a seq2seq OPUS-MT network, not an LLM. Decoding is greedy and
//! re-runs the decoder from the start at each step because the exported
//! decoder has no past-key-value inputs. Sentences that exceed the model input
//! limit fail explicitly instead of being silently truncated.

#![cfg(feature = "local-mt")]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use ort::session::Session;
use ort::value::Tensor;
use serde_json::Value;

use super::offline::{
    EngineInfo, EngineStatus, TranslatedSentence, TranslationEngine, TranslationEngineError,
};

const DEFAULT_EOS_TOKEN_ID: u32 = 0;
const DEFAULT_DECODER_START_TOKEN_ID: u32 = 65000;
const MAX_INPUT_TOKENS: usize = 512;
const MAX_NEW_TOKENS: usize = 512;
const ENCODER_FILES: &[&str] = &[
    "encoder_model_quantized.onnx",
    "encoder_model.onnx",
    "encoder_model_int8.onnx",
];
const DECODER_FILES: &[&str] = &[
    "decoder_model_quantized.onnx",
    "decoder_model.onnx",
    "decoder_model_int8.onnx",
];

pub struct OnnxMarianEngine {
    source_language: String,
    target_language: String,
    model_dir: PathBuf,
    // The environment owns the ONNX Runtime instance; sessions borrow from it.
    _environment: ort::environment::Environment,
    encoder: Mutex<Session>,
    decoder: Mutex<Session>,
    tokenizer: tokenizers::Tokenizer,
    eos_token_id: u32,
    decoder_start_token_id: u32,
}

fn inference_error(reason: &str) -> TranslationEngineError {
    TranslationEngineError::InferenceFailed(reason.to_string())
}

fn runtime_library_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    }
}

fn runtime_candidates(model_dir: &Path) -> Vec<PathBuf> {
    let models_root = model_dir.parent().unwrap_or(model_dir);
    let library = runtime_library_name();
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("ORT_DYLIB_PATH") {
        candidates.push(PathBuf::from(explicit));
    }
    candidates.push(models_root.join("runtime").join(library));
    candidates.push(model_dir.join("runtime").join(library));
    candidates.push(model_dir.join(library));
    candidates.push(models_root.join(library));
    candidates
}

static ENVIRONMENT: OnceLock<Result<ort::environment::Environment, String>> = OnceLock::new();

fn environment(
    model_dir: &Path,
) -> Result<&'static ort::environment::Environment, TranslationEngineError> {
    let initialized = ENVIRONMENT.get_or_init(|| {
        for candidate in runtime_candidates(model_dir) {
            if !candidate.is_file() {
                continue;
            }
            let builder = ort::init_from(&candidate)
                .map_err(|error| format!("onnxruntime_load_failed: {error}"))?;
            return builder
                .with_name("knowwise-local-mt")
                .build()
                .map_err(|error| format!("onnxruntime_init_failed: {error}"));
        }
        Err("onnxruntime_missing".to_string())
    });
    initialized
        .as_ref()
        .map_err(|_| inference_error("onnxruntime_unavailable"))
}

fn find_model_file(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    for base in [dir.to_path_buf(), dir.join("onnx")] {
        for name in names {
            let candidate = base.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The exported tokenizer.json from Transformers.js uses a `Precompiled`
/// normalizer with a null charsmap, which the Rust tokenizers crate rejects.
/// Dropping the normalizer is equivalent for the OPUS-MT vocabularies used
/// here and keeps the genuine token ids (we never substitute a different
/// tokenizer).
fn load_tokenizer(path: &Path) -> Result<tokenizers::Tokenizer, TranslationEngineError> {
    let bytes = std::fs::read(path).map_err(|_| inference_error("tokenizer_read_failed"))?;
    let mut value: Value =
        serde_json::from_slice(&bytes).map_err(|_| inference_error("tokenizer_json_invalid"))?;
    let null_charsmap = value
        .get("normalizer")
        .and_then(|normalizer| normalizer.get("precompiled_charsmap"))
        .map(Value::is_null)
        .unwrap_or(false);
    if null_charsmap {
        value["normalizer"] = Value::Null;
    }
    let patched =
        serde_json::to_vec(&value).map_err(|_| inference_error("tokenizer_json_invalid"))?;
    tokenizers::Tokenizer::from_bytes(patched).map_err(|_| inference_error("tokenizer_load_failed"))
}

fn read_json(path: &Path) -> Option<Value> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

fn token_id(config: &Option<Value>, key: &str) -> Option<u32> {
    config
        .as_ref()?
        .get(key)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
}

fn special_token_ids(dir: &Path) -> (u32, u32) {
    let generation = read_json(&dir.join("generation_config.json"));
    let config = read_json(&dir.join("config.json"));
    let eos = token_id(&generation, "eos_token_id")
        .or_else(|| token_id(&config, "eos_token_id"))
        .unwrap_or(DEFAULT_EOS_TOKEN_ID);
    let decoder_start = token_id(&generation, "decoder_start_token_id")
        .or_else(|| token_id(&config, "decoder_start_token_id"))
        .or_else(|| token_id(&generation, "pad_token_id"))
        .or_else(|| token_id(&config, "pad_token_id"))
        .unwrap_or(DEFAULT_DECODER_START_TOKEN_ID);
    (eos, decoder_start)
}

fn parse_direction(dir: &Path) -> Option<(String, String)> {
    let name = dir.file_name()?.to_str()?;
    let (source, target) = name.split_once('-')?;
    if source.is_empty() || target.is_empty() || source.contains('-') || target.contains('-') {
        return None;
    }
    Some((source.to_string(), target.to_string()))
}

/// Cheap status probe: the model files and a runtime library are present.
/// This does not load ONNX sessions, so the settings/status UI can report
/// readiness without paying the model-loading cost up front.
pub fn probe(dir: &Path) -> bool {
    parse_direction(dir).is_some()
        && find_model_file(dir, ENCODER_FILES).is_some()
        && find_model_file(dir, DECODER_FILES).is_some()
        && dir.join("tokenizer.json").is_file()
        && runtime_candidates(dir)
            .iter()
            .any(|candidate| candidate.is_file())
}

/// Create an engine from a complete model directory. Any missing file or
/// runtime returns `None`, which the caller reports as `BackendUnavailable`;
/// it never falls back to a fabricated translation.
pub fn create_engine(dir: &Path) -> Option<Box<dyn TranslationEngine>> {
    let (source_language, target_language) = parse_direction(dir)?;
    let encoder_path = find_model_file(dir, ENCODER_FILES)?;
    let decoder_path = find_model_file(dir, DECODER_FILES)?;
    let tokenizer_path = dir.join("tokenizer.json");
    if !tokenizer_path.is_file() {
        return None;
    }

    let tokenizer = load_tokenizer(&tokenizer_path).ok()?;
    let environment = environment(dir).ok()?;
    let mut encoder_builder = Session::builder(environment).ok()?;
    let encoder = encoder_builder.commit_from_file(&encoder_path).ok()?;
    let mut decoder_builder = Session::builder(environment).ok()?;
    let decoder = decoder_builder.commit_from_file(&decoder_path).ok()?;
    let (eos_token_id, decoder_start_token_id) = special_token_ids(dir);

    Some(Box::new(OnnxMarianEngine {
        source_language,
        target_language,
        model_dir: dir.to_path_buf(),
        _environment: environment.clone(),
        encoder: Mutex::new(encoder),
        decoder: Mutex::new(decoder),
        tokenizer,
        eos_token_id,
        decoder_start_token_id,
    }))
}

impl OnnxMarianEngine {
    fn translate_sentence(
        &self,
        original: &str,
        text: &str,
    ) -> Result<TranslatedSentence, TranslationEngineError> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|_| inference_error("tokenize_failed"))?;
        let input_ids: Vec<i64> = encoding
            .get_ids()
            .iter()
            .map(|&token| i64::from(token))
            .collect();
        if input_ids.is_empty() {
            return Err(inference_error("empty_input"));
        }
        if input_ids.len() > MAX_INPUT_TOKENS {
            return Err(TranslationEngineError::SentenceTooLong {
                chars: input_ids.len(),
                limit: MAX_INPUT_TOKENS,
            });
        }
        let source_len = input_ids.len();

        let encoder_inputs = ort::inputs![
            "input_ids" => Tensor::from_array((vec![1_i64, source_len as i64], input_ids))
                .map_err(|_| inference_error("encoder_input_failed"))?,
            "attention_mask" => Tensor::from_array((
                vec![1_i64, source_len as i64],
                vec![1_i64; source_len],
            ))
            .map_err(|_| inference_error("encoder_input_failed"))?,
        ];

        let (encoder_hidden, hidden_dim) = {
            let mut encoder = self
                .encoder
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let outputs = encoder
                .run(encoder_inputs)
                .map_err(|_| inference_error("encoder_run_failed"))?;
            let hidden = outputs
                .get("last_hidden_state")
                .ok_or_else(|| inference_error("encoder_output_missing"))?
                .try_extract_tensor::<f32>()
                .map_err(|_| inference_error("encoder_output_invalid"))?;
            let (shape, data) = hidden;
            if shape.len() != 3 || shape[2] <= 0 {
                return Err(inference_error("encoder_output_shape"));
            }
            (data.to_vec(), shape[2] as usize)
        };

        let mut decoder_ids: Vec<i64> = vec![i64::from(self.decoder_start_token_id)];
        let mut confidence_sum = 0.0_f32;
        let mut confidence_count = 0usize;
        let mut reached_eos = false;

        for _ in 0..MAX_NEW_TOKENS {
            let decoder_len = decoder_ids.len();
            let decoder_inputs = ort::inputs![
                "encoder_attention_mask" => Tensor::from_array((
                    vec![1_i64, source_len as i64],
                    vec![1_i64; source_len],
                ))
                .map_err(|_| inference_error("decoder_input_failed"))?,
                "input_ids" => Tensor::from_array((
                    vec![1_i64, decoder_len as i64],
                    decoder_ids.clone(),
                ))
                .map_err(|_| inference_error("decoder_input_failed"))?,
                "encoder_hidden_states" => Tensor::from_array((
                    vec![1_i64, source_len as i64, hidden_dim as i64],
                    encoder_hidden.clone(),
                ))
                .map_err(|_| inference_error("decoder_input_failed"))?,
            ];

            let (next_token, probability) = {
                let mut decoder = self
                    .decoder
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let outputs = decoder
                    .run(decoder_inputs)
                    .map_err(|_| inference_error("decoder_run_failed"))?;
                let logits = outputs
                    .get("logits")
                    .ok_or_else(|| inference_error("decoder_output_missing"))?
                    .try_extract_tensor::<f32>()
                    .map_err(|_| inference_error("decoder_output_invalid"))?;
                let (shape, data) = logits;
                if shape.len() != 3 || shape[2] <= 0 {
                    return Err(inference_error("decoder_output_shape"));
                }
                let vocabulary = shape[2] as usize;
                if data.len() < decoder_len * vocabulary {
                    return Err(inference_error("decoder_output_shape"));
                }
                let row = &data[(decoder_len - 1) * vocabulary..decoder_len * vocabulary];
                let mut best_index = 0usize;
                let mut best_logit = f32::NEG_INFINITY;
                for (index, &value) in row.iter().enumerate() {
                    if value > best_logit {
                        best_logit = value;
                        best_index = index;
                    }
                }
                if !best_logit.is_finite() {
                    return Err(inference_error("decoder_logits_invalid"));
                }
                let sum: f32 = row.iter().map(|value| (value - best_logit).exp()).sum();
                let probability = if sum.is_finite() && sum > 0.0 {
                    1.0 / sum
                } else {
                    0.0
                };
                (best_index as u32, probability)
            };

            confidence_sum += probability;
            confidence_count += 1;
            decoder_ids.push(i64::from(next_token));
            if next_token == self.eos_token_id {
                reached_eos = true;
                break;
            }
        }

        if !reached_eos {
            return Err(inference_error("max_new_tokens_reached"));
        }
        let token_ids: Vec<u32> = decoder_ids.iter().map(|&id| id as u32).collect();
        let translated = self
            .tokenizer
            .decode(&token_ids, true)
            .map_err(|_| inference_error("decode_failed"))?;
        if translated.trim().is_empty() {
            return Err(inference_error("empty_translation"));
        }
        let confidence = if confidence_count == 0 {
            0.0
        } else {
            (confidence_sum / confidence_count as f32).clamp(0.0, 1.0)
        };
        Ok(TranslatedSentence {
            source: original.to_string(),
            text: translated,
            confidence,
        })
    }
}

impl TranslationEngine for OnnxMarianEngine {
    fn info(&self, _source: &str, _target: &str) -> EngineInfo {
        EngineInfo {
            status: EngineStatus::Ready,
            engine_name: format!(
                "opus-mt (onnx, {}-{})",
                self.source_language, self.target_language
            ),
            source_language: self.source_language.clone(),
            target_language: self.target_language.clone(),
            model_dir: Some(self.model_dir.clone()),
        }
    }

    fn translate_batch(
        &self,
        _source_language: &str,
        _target_language: &str,
        sentences: &[String],
    ) -> Result<Vec<TranslatedSentence>, TranslationEngineError> {
        let mut translated = Vec::with_capacity(sentences.len());
        for sentence in sentences {
            let text = sentence.trim();
            if text.is_empty() {
                translated.push(TranslatedSentence {
                    source: sentence.clone(),
                    text: String::new(),
                    confidence: 1.0,
                });
                continue;
            }
            translated.push(self.translate_sentence(sentence, text)?);
        }
        Ok(translated)
    }
}
