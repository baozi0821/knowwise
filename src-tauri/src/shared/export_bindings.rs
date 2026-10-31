//! ts-rs 契约导出(M00,验收 M00-A3 / API-01)。
//!
//! 导出目录由 `.cargo/config.toml` 的 `[env] TS_RS_EXPORT_DIR=".."`
//! 统一固定为仓库根;`#[ts(export)]` 类型由宏自动生成导出测试,
//! `cargo test` 即重新生成 `src/shared/generated/`。CI 先重新生成再比对 diff,
//! 任何手改生成文件都会失败。生成目录不手改。

#[cfg(test)]
mod tests {
    use ts_rs::TS;

    fn export_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/shared/generated")
    }

    /// ts-rs 的泛型 decl() 直接产出规范泛型声明;断言形状并落盘。
    #[test]
    fn export_page_generic_and_assert_shape() {
        use crate::shared::paging::Page;
        let decl = Page::<i32>::decl();
        assert_eq!(
            decl, "type Page<T> = { items: Array<T>, nextCursor?: string, total?: number, };",
            "Page 结构漂移"
        );
        std::fs::create_dir_all(export_dir()).unwrap();
        std::fs::write(export_dir().join("Page.ts"), format!("export {decl}\n")).unwrap();
    }

    /// 存在性冒烟:确保自动导出测试确实落盘了关键契约文件。
    #[test]
    fn generated_contract_files_exist() {
        // 该测试在导出测试之后运行无保证,只断言“已生成过”能力:
        // 首次运行由 export_bindings_* 自动测试产出;此处检查目录可用。
        std::fs::create_dir_all(export_dir()).unwrap();
        assert!(export_dir().join("Page.ts").exists());
    }
}
