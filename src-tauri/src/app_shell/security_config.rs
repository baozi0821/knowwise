//! 安全配置契约(M13,《M13》§10;验收 M13-A6)。
//!
//! 读取 `tauri.conf.json` 与 `capabilities/` 的实际内容做静态断言:
//! 生产 CSP 禁止任意脚本/eval/远程连接;开发热更新放行只存在于
//! `devCsp`,不进入发布配置;能力清单只开放最小插件权限,
//! 不提供 fs/http/shell 直通。默认特性(无 Tauri)下可测试。

use std::collections::BTreeMap;
use std::path::Path;

/// 解析 CSP 为指令 → 来源列表。不处理 nonce/哈希细节,只按分号/空格切分。
pub fn parse_csp(csp: &str) -> BTreeMap<String, Vec<String>> {
    let mut map = BTreeMap::new();
    for directive in csp.split(';') {
        let directive = directive.trim();
        if directive.is_empty() {
            continue;
        }
        let mut parts = directive.split_whitespace();
        let Some(name) = parts.next() else { continue };
        map.insert(name.to_string(), parts.map(|s| s.to_string()).collect());
    }
    map
}

/// 来源是否为远程网络来源(http/https/wss/ws 协议且非本机 IPC 白名单)。
pub fn is_remote_source(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    // IPC 通道是 Tauri WebView ↔ Rust 的本机回环,不是外部网络。
    if lower == "http://ipc.localhost" || lower == "ipc:" {
        return false;
    }
    // HMR 仅允许本机回环;其余网络协议一律视为远程。
    if let Some(rest) = lower.strip_prefix("ws://") {
        return !rest.starts_with("localhost:") && !rest.starts_with("127.0.0.1:");
    }
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("wss://")
}

/// CSP 中是否存在任意远程来源(逐指令检查)。
pub fn remote_sources(csp: &str) -> Vec<String> {
    let directives = parse_csp(csp);
    let mut found = Vec::new();
    for (name, sources) in &directives {
        for s in sources {
            if is_remote_source(s) {
                found.push(format!("{name} {s}"));
            }
        }
    }
    found
}

/// 能力文件允许携带的最大权限集合(M13 冻结):业务全部走自定义命令,
/// 只保留对话框(导入/导出选择)、剪贴板写入(复制导出)与
/// 受控外部链接打开。新增权限必须先在本清单登记并说明理由。
pub const ALLOWED_CAPABILITY_PERMISSIONS: &[&str] = &[
    "core:default",
    "dialog:allow-open",
    "dialog:allow-save",
    "clipboard-manager:allow-write-text",
    "opener:allow-open-url",
];

/// 读取 capabilities 目录中全部能力文件声明的权限。
pub fn collect_capability_permissions(dir: &Path) -> Result<BTreeMap<String, Vec<String>>, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("读取 capabilities 目录失败:{e}"))?;
    let mut result = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let content =
            std::fs::read_to_string(&path).map_err(|e| format!("读取 {:?} 失败:{e}", name))?;
        let json: serde_json::Value =
            serde_json::from_str(&content).map_err(|e| format!("解析 {:?} 失败:{e}", name))?;
        let permissions = json
            .get("permissions")
            .and_then(|p| p.as_array())
            .ok_or_else(|| format!("能力文件 {name} 缺少 permissions 数组"))?;
        let list = permissions
            .iter()
            .map(|p| {
                p.as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{p:?}"))
            })
            .collect();
        result.insert(name, list);
    }
    if result.is_empty() {
        return Err("capabilities 目录为空".to_string());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
    }

    fn load_conf() -> serde_json::Value {
        let text =
            std::fs::read_to_string(repo_root().join("tauri.conf.json")).expect("tauri.conf.json");
        serde_json::from_str(&text).expect("tauri.conf.json 解析失败")
    }

    fn production_csp() -> String {
        load_conf()["app"]["security"]["csp"]
            .as_str()
            .expect("生产 CSP 必须是字符串,null 不允许")
            .to_string()
    }

    fn dev_csp() -> String {
        load_conf()["app"]["security"]["devCsp"]
            .as_str()
            .expect("devCsp 必须显式声明(开发放行不得落在生产 csp)")
            .to_string()
    }

    /// M13-A6:生产 CSP 阻断外部网络、任意路径与脚本注入。
    #[test]
    fn production_csp_blocks_remote_and_eval() {
        let csp = production_csp();
        let remote = remote_sources(&csp);
        assert!(
            remote.is_empty(),
            "生产 CSP 出现远程来源:{remote:?}(仅 connect-src 允许 http://ipc.localhost)"
        );
        let directives = parse_csp(&csp);
        let default = directives.get("default-src").expect("default-src 必填");
        assert!(
            default.iter().all(|s| s == "'self'"),
            "default-src 只允许 'self':{default:?}"
        );
        let script = directives.get("script-src").expect("script-src 必填");
        assert!(
            !script.iter().any(|s| s.contains("*")),
            "script-src 不允许通配符:{script:?}"
        );
        assert!(
            !script.iter().any(|s| s == "'unsafe-eval'"),
            "script-src 禁止 unsafe-eval:{script:?}"
        );
        assert!(
            !script.iter().any(|s| s == "'unsafe-inline'"),
            "生产 script-src 禁止 unsafe-inline:{script:?}"
        );
        assert_eq!(
            directives.get("object-src").map(|s| s.as_slice()),
            Some(["'none'".to_string()].as_slice()),
            "object-src 必须为 'none'"
        );
        assert_eq!(
            directives.get("base-uri").map(|s| s.as_slice()),
            Some(["'self'".to_string()].as_slice()),
            "base-uri 必须为 'self'"
        );
    }

    /// 开发热更新配置不得进入发布包:devCsp 与 csp 必须分离,
    /// 且 devCsp 的网络放行仅限本机回环。
    #[test]
    fn dev_csp_is_separate_and_localhost_only() {
        let dev = dev_csp();
        let prod = production_csp();
        assert_ne!(dev, prod, "devCsp 必须与生产 csp 不同");
        assert!(
            dev.contains("ws://localhost:5173"),
            "devCsp 需要为 Vite HMR 放行本机 websocket"
        );
        for source in remote_sources(&dev) {
            assert!(
                source.contains("localhost:") || source.contains("127.0.0.1:"),
                "devCsp 的网络来源只允许本机回环:{source}"
            );
        }
        // 生产配置没有 ws: 放行。
        assert!(
            !prod.to_ascii_lowercase().contains("ws://"),
            "生产 CSP 不得包含 ws:// 放行:{prod}"
        );
    }

    /// M13-A6:能力清单最小化,无 fs/http/shell 直通。
    #[test]
    fn capabilities_are_minimal() {
        let caps = collect_capability_permissions(&repo_root().join("capabilities"))
            .expect("capabilities 解析失败");
        for (file, permissions) in &caps {
            for p in permissions {
                assert!(
                    ALLOWED_CAPABILITY_PERMISSIONS.contains(&p.as_str()),
                    "能力文件 {file} 携带未登记权限 {p};如确有必要请更新 ALLOWED_CAPABILITY_PERMISSIONS 并说明理由"
                );
                for banned_prefix in ["fs:", "shell:", "http:", "core:webview:", "core:window:"] {
                    assert!(
                        !p.starts_with(banned_prefix),
                        "能力文件 {file} 禁止直通权限 {p}"
                    );
                }
            }
        }
    }

    #[test]
    fn csp_parser_semantics() {
        let parsed = parse_csp(
            "default-src 'self'; script-src 'self'; connect-src 'self' ipc: http://ipc.localhost",
        );
        assert_eq!(parsed["default-src"], vec!["'self'"]);
        assert_eq!(
            parsed["connect-src"],
            vec!["'self'", "ipc:", "http://ipc.localhost"]
        );
        assert!(is_remote_source("https://evil.example"));
        assert!(is_remote_source("http://192.168.1.4/x"));
        assert!(!is_remote_source("http://ipc.localhost"));
        assert!(!is_remote_source("ipc:"));
        assert!(!is_remote_source("'self'"));
        assert!(!is_remote_source("ws://localhost:5173"));
        assert!(is_remote_source("ws://example.dev"));
        assert_eq!(
            remote_sources("img-src https://a.example; connect-src http://ipc.localhost"),
            vec!["img-src https://a.example".to_string()]
        );
    }
}
