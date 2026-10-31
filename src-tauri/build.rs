use std::path::{Path, PathBuf};

fn main() {
    // 仅桌面 feature 走 Tauri 资源构建;核心构建(含全部测试)跳过。
    if std::env::var("CARGO_FEATURE_DESKTOP").is_ok() {
        tauri_build::build();
    }
    deploy_webview2_loader_for_gnu();
}

/// windows-gnu 目标下 webview2-com-sys 动态链接 `WebView2Loader.dll`
/// (MSVC 目标为静态链接,此函数直接跳过)。Windows 按可执行文件所在目录
/// 优先搜索 DLL,因此把 crate 包内自带的 DLL 复制到 target/{profile} 与
/// target/{profile}/deps,使 `cargo test` / `cargo run` 可直接启动。
/// 这是 M00 在 windows-gnu 工具链上的已验证工程决策,见 README。
fn deploy_webview2_loader_for_gnu() {
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if target_env != "gnu" || target_arch != "x86_64" {
        return;
    }
    let Some(dll) = locate_bundled_loader(&target_arch) else {
        println!(
            "cargo:warning=WebView2Loader.dll not found in registry cache; tests may fail to start"
        );
        return;
    };
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest_dir.join("target"));
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    for dir in [
        target_dir.join(&profile),
        target_dir.join(&profile).join("deps"),
    ] {
        let _ = std::fs::create_dir_all(&dir);
        let dest = dir.join("WebView2Loader.dll");
        if needs_copy(&dll, &dest) {
            if let Err(e) = std::fs::copy(&dll, &dest) {
                println!(
                    "cargo:warning=failed to deploy WebView2Loader.dll to {}: {e}",
                    dest.display()
                );
            }
        }
    }
}

fn locate_bundled_loader(_arch: &str) -> Option<PathBuf> {
    let cargo_home = std::env::var("CARGO_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("USERPROFILE").unwrap_or_else(|_| String::from("."));
            Path::new(&home).join(".cargo")
        });
    let registry_src = cargo_home.join("registry").join("src");
    let entries = std::fs::read_dir(&registry_src).ok()?;
    for host_dir in entries.flatten() {
        let crates_dir = host_dir.path();
        let Ok(crates) = std::fs::read_dir(&crates_dir) else {
            continue;
        };
        for c in crates.flatten() {
            let name = c.file_name().to_string_lossy().to_string();
            if name.starts_with("webview2-com-sys-") {
                let dll = c.path().join("x64").join("WebView2Loader.dll");
                if dll.exists() {
                    return Some(dll);
                }
            }
        }
    }
    None
}

/// 简单按大小判断是否需要复制,避免每次构建都写文件触发额外 IO。
fn needs_copy(src: &Path, dest: &Path) -> bool {
    match (std::fs::metadata(src), std::fs::metadata(dest)) {
        (Ok(s), Ok(d)) => s.len() != d.len(),
        _ => true,
    }
}
