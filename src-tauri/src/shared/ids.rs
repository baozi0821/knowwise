//! ID 生成端口(M00)。ID 一律由 Rust 核心生成,前端不得自造业务 ID。

use uuid::Uuid;

/// 实体 ID 与确定性 ID 的唯一来源。
pub trait IdGenerator: Send + Sync {
    /// 普通实体 ID:UUIDv7 字符串(小写连字符格式)。
    fn new_entity_id(&self) -> String;

    /// 解析块 ID:`UUIDv5(NS, parseGenerationId + ":" + blockKey)`。
    /// 同一代次内 blockKey 稳定 ⇒ 重试产生相同块 ID(去重与幂等的基础)。
    fn parser_block_id(&self, parse_generation_id: &str, block_key: &str) -> String {
        let name = format!("{parse_generation_id}:{block_key}");
        Uuid::new_v5(&crate::shared::KNOWWISE_NS, name.as_bytes()).to_string()
    }

    /// 按需选区块 ID:由文档版本、规范化文本哈希与规范化定位计算。
    fn on_demand_block_id(
        &self,
        document_version_id: &str,
        normalized_text_hash: &str,
        canonical_locator: &str,
    ) -> String {
        let name = format!("{document_version_id}|{normalized_text_hash}|{canonical_locator}");
        Uuid::new_v5(&crate::shared::KNOWWISE_NS, name.as_bytes()).to_string()
    }

    /// 256 位随机令牌(访问令牌 / 解析桥令牌),十六进制小写。
    fn new_token(&self) -> String;
}

pub struct SystemIdGenerator;

impl IdGenerator for SystemIdGenerator {
    fn new_entity_id(&self) -> String {
        Uuid::now_v7().to_string()
    }

    fn new_token(&self) -> String {
        use rand::RngCore;
        let mut buf = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut buf);
        hex::encode(buf)
    }
}

/// 测试用确定性生成器:同序列可重放,断言稳定 ID。
pub struct TestIdGenerator {
    counter: std::sync::atomic::AtomicU64,
}

impl TestIdGenerator {
    pub fn new() -> Self {
        Self {
            counter: std::sync::atomic::AtomicU64::new(1),
        }
    }
}

impl Default for TestIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGenerator for TestIdGenerator {
    fn new_entity_id(&self) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("test-id-{n:08}")
    }

    fn new_token(&self) -> String {
        let n = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("test-token-{n:08}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_block_id_is_deterministic_and_input_sensitive() {
        let g = SystemIdGenerator;
        let a = g.parser_block_id("gen-1", "para-3");
        let b = g.parser_block_id("gen-1", "para-3");
        let c = g.parser_block_id("gen-2", "para-3");
        assert_eq!(a, b, "同代次同 blockKey 必须产生相同块 ID");
        assert_ne!(a, c, "不同代次必须产生不同块 ID");
    }

    #[test]
    fn on_demand_block_id_binds_version_text_locator() {
        let g = SystemIdGenerator;
        let base = g.on_demand_block_id("v1", "h1", "loc1");
        assert_eq!(base, g.on_demand_block_id("v1", "h1", "loc1"));
        assert_ne!(base, g.on_demand_block_id("v1", "h2", "loc1"));
        assert_ne!(base, g.on_demand_block_id("v2", "h1", "loc1"));
    }

    #[test]
    fn entity_ids_are_unique_and_v7() {
        let g = SystemIdGenerator;
        let a = Uuid::parse_str(&g.new_entity_id()).expect("valid uuid");
        let b = Uuid::parse_str(&g.new_entity_id()).expect("valid uuid");
        assert_ne!(a, b);
        assert_eq!(a.get_version_num(), 7);
    }
}
