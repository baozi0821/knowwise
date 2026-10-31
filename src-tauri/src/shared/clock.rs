//! 时间端口(M00)。时间只在 Rust 采集并写库;测试可替换。

use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    /// UTC epoch 毫秒(数据库时间字段的唯一来源)。
    fn now_ms(&self) -> i64;
    /// 单调时钟毫秒(耗时统计;不受系统时间调整影响)。
    fn monotonic_ms(&self) -> u64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn monotonic_ms(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;
        static START: OnceLock<Instant> = OnceLock::new();
        let start = START.get_or_init(Instant::now);
        start.elapsed().as_millis() as u64
    }
}

/// 测试时钟:可设定、可推进。
pub struct TestClock {
    now: std::sync::atomic::AtomicI64,
}

impl TestClock {
    pub fn new(start_ms: i64) -> Self {
        Self {
            now: std::sync::atomic::AtomicI64::new(start_ms),
        }
    }

    pub fn set(&self, ms: i64) {
        self.now.store(ms, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_ms(&self) -> i64 {
        self.now.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn monotonic_ms(&self) -> u64 {
        self.now.load(std::sync::atomic::Ordering::SeqCst).max(0) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_advances() {
        let c = TestClock::new(1000);
        assert_eq!(c.now_ms(), 1000);
        c.advance(250);
        assert_eq!(c.now_ms(), 1250);
    }

    #[test]
    fn system_clock_is_epoch_sane() {
        let c = SystemClock;
        let now = c.now_ms();
        // 2026-01-01 之后,避免明显回归。
        assert!(now > 1_767_225_600_000, "system clock: {now}");
    }
}
