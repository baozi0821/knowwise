// Manual probe for the optional local OPUS-MT backend.
//
// Build and run:
//   cargo run --example local_mt_probe --features local-mt -- <model-dir> [text]
//
// The model directory must contain tokenizer.json plus encoder/decoder ONNX
// files. The ONNX Runtime library can be supplied with ORT_DYLIB_PATH, or
// placed at <models-root>/runtime/.

#[cfg(feature = "local-mt")]
fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let dir = args
        .get(1)
        .ok_or_else(|| "usage: local_mt_probe <model-dir> [text]".to_string())?;
    let text = args
        .get(2)
        .cloned()
        .unwrap_or_else(|| "你好，世界。".to_string());
    let engine = knowwise_lib::translation::onnx_mt::create_engine(std::path::Path::new(dir))
        .ok_or_else(|| {
            "engine unavailable: check ORT_DYLIB_PATH, tokenizer.json, and ONNX files".to_string()
        })?;
    let translated = engine
        .translate_batch("auto", "en", &[text])
        .map_err(|error| format!("{error:?}"))?;
    println!("{translated:?}");
    Ok(())
}

#[cfg(not(feature = "local-mt"))]
fn main() {
    eprintln!("build with --features local-mt");
}
