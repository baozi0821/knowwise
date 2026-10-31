//! 临时流事件桥(`stream_event.v1`,desktop feature)。
//!
//! 使用 Tauri v2 Channel 而不是全局事件总线:Channel 保序、适合高频增量;
//! 事件只做临时展示,不写数据库。最终事实仍是消息表里的
//! `answer.v1` / `agent.v1` / `assistant_notice.v1`。
//!
//! 看门狗(`RunWatchdog`)只依据真实计时与真实事件决定心跳/通知:
//! 它不是模型推理,也不按秒轮播固定文案。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::shared::stream::{StreamObserver, StreamStage};

pub const STREAM_EVENT_SCHEMA_VERSION: &str = "stream_event.v1";

/// 正文增量合并窗口(毫秒):provider 常按 5–15ms 吐块,逐块过 Channel 只是
/// 把 IPC 与前端重渲都打满。合并不改内容/顺序,也不伪造进度。
pub const ANSWER_COALESCE_MS: std::time::Duration = std::time::Duration::from_millis(24);
/// 缓冲块数上限:慢速网络上长时间不触发也会定期发出,避免用户看不到进展。
pub const ANSWER_COALESCE_MAX_CHUNKS: usize = 24;

/// 待发正文缓冲。
#[derive(Default)]
struct PendingAnswer {
    text: String,
    chunks: usize,
}

/// 单条流事件(前端 reducer 消费)。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamEventV1 {
    pub schema_version: &'static str,
    pub seq: u64,
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visible_message: Option<String>,
}

/// 一次运行的可观测指标(只含 ID/状态/耗时,不含正文/密钥/路径)。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunMetricsSnapshot {
    pub mode: String,
    pub first_token_ms: Option<i64>,
    pub first_visible_answer_ms: Option<i64>,
    pub reasoning_first_event_ms: Option<i64>,
    pub max_event_gap_ms: i64,
    pub first_answer_ms: Option<i64>,
    pub total_ms: Option<i64>,
    pub fallback_used: bool,
    pub citation_valid_rate: Option<f64>,
}

#[derive(Debug, Default)]
struct Timing {
    last_event_ms: i64,
    max_gap_ms: i64,
    first_token_ms: Option<i64>,
    first_visible_answer_ms: Option<i64>,
    reasoning_first_event_ms: Option<i64>,
    total_ms: Option<i64>,
    fallback_used: bool,
    citation_valid_rate: Option<f64>,
    mode: String,
}

/// 把真实执行事件写入 Tauri Channel。
pub struct ChannelObserver {
    channel: tauri::ipc::Channel<StreamEventV1>,
    seq: AtomicU64,
    message_id: String,
    task_id: Mutex<Option<String>>,
    started: Instant,
    stage: Mutex<StreamStage>,
    visible: AtomicBool,
    terminal: AtomicBool,
    final_status: Mutex<Option<String>>,
    reasoning_text: Mutex<String>,
    timing: Mutex<Timing>,
    pending_answer: Mutex<PendingAnswer>,
    last_answer_emit: Mutex<Option<Instant>>,
}

impl ChannelObserver {
    pub fn new(channel: tauri::ipc::Channel<StreamEventV1>, message_id: &str) -> Self {
        Self {
            channel,
            seq: AtomicU64::new(0),
            message_id: message_id.to_string(),
            task_id: Mutex::new(None),
            started: Instant::now(),
            stage: Mutex::new(StreamStage::Preparing),
            visible: AtomicBool::new(false),
            terminal: AtomicBool::new(false),
            final_status: Mutex::new(None),
            reasoning_text: Mutex::new(String::new()),
            timing: Mutex::new(Timing::default()),
            pending_answer: Mutex::new(PendingAnswer::default()),
            last_answer_emit: Mutex::new(None),
        }
    }

    pub fn message_id(&self) -> &str {
        &self.message_id
    }

    pub fn set_task_id(&self, task_id: &str) {
        *self.task_id.lock().unwrap() = Some(task_id.to_string());
    }

    pub fn elapsed_ms(&self) -> i64 {
        self.started.elapsed().as_millis().min(i64::MAX as u128) as i64
    }

    pub fn stage(&self) -> StreamStage {
        *self.stage.lock().unwrap()
    }

    pub fn has_visible_content(&self) -> bool {
        self.visible.load(Ordering::SeqCst)
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal.load(Ordering::SeqCst)
    }

    /// 终态:verified / unverified / failed / canceled。
    pub fn final_status(&self) -> Option<String> {
        self.final_status.lock().unwrap().clone()
    }

    /// 本次运行收到的 provider 原始 reasoning(仅内存;是否落库由设置决定)。
    pub fn reasoning_text(&self) -> String {
        self.reasoning_text.lock().unwrap().clone()
    }

    /// 距离上一条事件(含心跳)的时间;用于心跳判定。
    pub fn gap_since_last_event_ms(&self) -> i64 {
        let now = self.elapsed_ms();
        let timing = self.timing.lock().unwrap();
        if timing.last_event_ms <= 0 {
            now
        } else {
            (now - timing.last_event_ms).max(0)
        }
    }

    pub fn mark_fallback(&self) {
        self.timing.lock().unwrap().fallback_used = true;
    }

    pub fn set_citation_valid_rate(&self, rate: Option<f64>) {
        self.timing.lock().unwrap().citation_valid_rate = rate;
    }

    pub fn metrics(&self) -> RunMetricsSnapshot {
        let timing = self.timing.lock().unwrap();
        RunMetricsSnapshot {
            mode: timing.mode.clone(),
            first_token_ms: timing.first_token_ms,
            first_visible_answer_ms: timing.first_visible_answer_ms,
            reasoning_first_event_ms: timing.reasoning_first_event_ms,
            max_event_gap_ms: timing.max_gap_ms,
            first_answer_ms: timing.first_visible_answer_ms,
            total_ms: timing.total_ms,
            fallback_used: timing.fallback_used,
            citation_valid_rate: timing.citation_valid_rate,
        }
    }

    fn emit(&self, event: StreamEventV1) {
        let _ = self.channel.send(event);
    }

    fn mark_visible(&self, reasoning: bool, answer: bool) {
        self.visible.store(true, Ordering::SeqCst);
        let now = self.elapsed_ms();
        let mut timing = self.timing.lock().unwrap();
        if timing.first_token_ms.is_none() {
            timing.first_token_ms = Some(now);
        }
        if reasoning && timing.reasoning_first_event_ms.is_none() {
            timing.reasoning_first_event_ms = Some(now);
        }
        if answer && timing.first_visible_answer_ms.is_none() {
            timing.first_visible_answer_ms = Some(now);
        }
    }

    fn base(&self, kind: &'static str) -> StreamEventV1 {
        let now = self.elapsed_ms();
        {
            let mut timing = self.timing.lock().unwrap();
            if timing.last_event_ms > 0 {
                timing.max_gap_ms = timing.max_gap_ms.max(now - timing.last_event_ms);
            }
            timing.last_event_ms = now;
        }
        StreamEventV1 {
            schema_version: STREAM_EVENT_SCHEMA_VERSION,
            seq: self.seq.fetch_add(1, Ordering::SeqCst) + 1,
            kind,
            message_id: Some(self.message_id.clone()),
            task_id: self.task_id.lock().unwrap().clone(),
            stage: None,
            text: None,
            tool: None,
            status: None,
            summary: None,
            elapsed_ms: Some(now),
            error_code: None,
            visible_message: None,
        }
    }

    /// 把正文增量并入待发缓冲;返回 true 表示本轮不立即发送。
    ///
    /// 节流规则(纯时间窗,无后台线程):
    /// - 距上次发送已超过 [`ANSWER_COALESCE_MS`],或缓冲已累积到上限,
    ///   就一次性发出(内容按序拼接,与逐块发送等价);
    /// - 否则继续累积,由下一次到达或终态 `flush_answer()` 发出。
    ///
    /// 这样既不会丢字符、也不改变顺序,更不会伪造进度。
    fn coalesce_answer(&self, text: &str) -> bool {
        let mut pending = self.pending_answer.lock().unwrap();
        pending.text.push_str(text);
        pending.chunks += 1;
        let now = Instant::now();
        let elapsed = self
            .last_answer_emit
            .lock()
            .unwrap()
            .map(|at| now.duration_since(at))
            .unwrap_or_default();
        let due = elapsed >= ANSWER_COALESCE_MS || pending.chunks >= ANSWER_COALESCE_MAX_CHUNKS;
        if !due {
            return true;
        }
        self.last_answer_emit.lock().unwrap().replace(now);
        pending.chunks = 0;
        let text = std::mem::take(&mut pending.text);
        drop(pending);
        let mut event = self.base("answer.delta");
        event.text = Some(text);
        self.emit(event);
        true
    }

    /// 立即发送缓冲中的正文增量(终态/重置前调用,保持顺序与完整性)。
    fn flush_answer(&self) {
        let text = {
            let mut pending = self.pending_answer.lock().unwrap();
            pending.chunks = 0;
            std::mem::take(&mut pending.text)
        };
        if text.is_empty() {
            return;
        }
        self.last_answer_emit
            .lock()
            .unwrap()
            .replace(Instant::now());
        let mut event = self.base("answer.delta");
        event.text = Some(text);
        self.emit(event);
    }

    pub fn run_started(&self, mode: &str) {
        self.timing.lock().unwrap().mode = mode.to_string();
        let mut event = self.base("run.started");
        event.status = Some(mode.to_string());
        self.emit(event);
    }

    pub fn run_finished(&self, verified: bool) {
        self.flush_answer();
        self.timing.lock().unwrap().total_ms = Some(self.elapsed_ms());
        self.terminal.store(true, Ordering::SeqCst);
        let status = if verified { "verified" } else { "unverified" };
        *self.final_status.lock().unwrap() = Some(status.to_string());
        let mut event = self.base("run.finished");
        event.status = Some(status.into());
        self.emit(event);
    }

    pub fn run_failed(&self, error_code: &str, visible_message: &str) {
        self.flush_answer();
        self.timing.lock().unwrap().total_ms = Some(self.elapsed_ms());
        self.terminal.store(true, Ordering::SeqCst);
        let status = if error_code == "TASK_CANCELED" {
            "canceled"
        } else {
            "failed"
        };
        *self.final_status.lock().unwrap() = Some(status.to_string());
        let mut event = self.base("run.failed");
        event.status = Some("failed".into());
        event.error_code = Some(error_code.to_string());
        event.visible_message = Some(visible_message.to_string());
        self.emit(event);
    }
}

impl StreamObserver for ChannelObserver {
    fn on_stage(&self, stage: StreamStage) {
        self.flush_answer();
        *self.stage.lock().unwrap() = stage;
        let mut event = self.base("stage.changed");
        event.stage = Some(stage.as_str().to_string());
        self.emit(event);
    }

    fn on_reasoning(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.mark_visible(true, false);
        {
            let mut buffer = self.reasoning_text.lock().unwrap();
            // 上限 256k 字符:只用于用户显式开启的本地保存,避免异常流撑爆内存。
            if buffer.chars().count() < 262_144 {
                buffer.push_str(text);
            }
        }
        let mut event = self.base("reasoning.delta");
        event.text = Some(text.to_string());
        self.emit(event);
    }

    fn on_answer(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.mark_visible(false, true);
        // 按 ~24ms 合并连续增量:provider 常按 5–15ms 吐出小块,逐块过 IPC
        // 会把 Channel 打满,前端也只是每帧重渲一次。合并不改变内容与顺序
        // (拼接后等价于逐块追加),也不会伪造进度;终态事件会先清空缓冲。
        if self.coalesce_answer(text) {
            return;
        }
        let mut event = self.base("answer.delta");
        event.text = Some(text.to_string());
        self.emit(event);
    }

    fn on_answer_reset(&self, reason: &str) {
        self.flush_answer();
        let mut event = self.base("answer.reset");
        event.summary = Some(reason.to_string());
        self.emit(event);
    }

    fn on_notice(&self, visible_message: &str) {
        if visible_message.is_empty() {
            return;
        }
        let mut event = self.base("notice");
        event.visible_message = Some(visible_message.to_string());
        self.emit(event);
    }

    fn on_tool_started(&self, step_id: &str, tool: &str, summary: &str) {
        self.visible.store(true, Ordering::SeqCst);
        let mut event = self.base("tool.started");
        event.tool = Some(tool.to_string());
        event.status = Some("running".into());
        event.summary = Some(summary.to_string());
        event.text = Some(step_id.to_string());
        self.emit(event);
    }

    fn on_tool_finished(&self, step_id: &str, tool: &str, status: &str, summary: &str) {
        let mut event = self.base("tool.finished");
        event.tool = Some(tool.to_string());
        event.status = Some(status.to_string());
        event.summary = Some(summary.to_string());
        event.text = Some(step_id.to_string());
        self.emit(event);
    }

    fn on_heartbeat(&self, stage: StreamStage, elapsed_ms: i64) {
        let mut event = self.base("heartbeat");
        event.stage = Some(stage.as_str().to_string());
        event.elapsed_ms = Some(elapsed_ms);
        self.emit(event);
    }
}

/// 看门狗动作(纯逻辑,便于单测)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchdogAction {
    None,
    Heartbeat,
    Notice,
}

/// 质量模式事件间隔上限 / 无可见内容 deadline。
pub const HEARTBEAT_INTERVAL_MS: i64 = 2_000;
pub const FAST_VISIBLE_DEADLINE_MS: i64 = 6_000;
pub const QUALITY_VISIBLE_DEADLINE_MS: i64 = 15_000;

/// 计算下一次看门狗动作。`notice_sent` 保证只发一次真实通知。
pub fn watchdog_action(
    mode: &str,
    elapsed_ms: i64,
    gap_ms: i64,
    has_visible: bool,
    notice_sent: bool,
) -> WatchdogAction {
    if !has_visible {
        let deadline = if mode == "quality" {
            QUALITY_VISIBLE_DEADLINE_MS
        } else {
            FAST_VISIBLE_DEADLINE_MS
        };
        if !notice_sent && elapsed_ms >= deadline {
            return WatchdogAction::Notice;
        }
    }
    if mode == "quality" && gap_ms >= HEARTBEAT_INTERVAL_MS {
        return WatchdogAction::Heartbeat;
    }
    WatchdogAction::None
}

/// UI 关闭不会阻止后端终态;该线程只发送临时事件,不写数据库。
pub struct RunWatchdog;

impl RunWatchdog {
    /// 启动分离线程:观察者进入终态后自行退出;最多存活 10 分钟。
    pub fn start(observer: Arc<ChannelObserver>, mode: &str) {
        let mode = mode.to_string();
        std::thread::spawn(move || {
            let mut notice_sent = false;
            let max_lifetime = Duration::from_secs(600);
            let started = Instant::now();
            while !observer.is_terminal() && started.elapsed() < max_lifetime {
                std::thread::sleep(Duration::from_millis(100));
                if observer.is_terminal() {
                    break;
                }
                match watchdog_action(
                    &mode,
                    observer.elapsed_ms(),
                    observer.gap_since_last_event_ms(),
                    observer.has_visible_content(),
                    notice_sent,
                ) {
                    WatchdogAction::Heartbeat => {
                        observer.on_heartbeat(observer.stage(), observer.elapsed_ms());
                    }
                    WatchdogAction::Notice => {
                        notice_sent = true;
                        observer.on_notice(if mode == "quality" {
                            "模型尚未返回可见内容;仍在生成,可随时取消,完成后会显示真实结果。"
                        } else {
                            "模型尚未返回可见答案;仍在生成,可随时取消,完成后会显示真实结果。"
                        });
                    }
                    WatchdogAction::None => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_heartbeats_only_after_two_second_gap() {
        assert_eq!(
            watchdog_action("quality", 2_100, 1_999, true, false),
            WatchdogAction::None
        );
        assert_eq!(
            watchdog_action("quality", 2_100, 2_000, true, false),
            WatchdogAction::Heartbeat
        );
        assert_eq!(
            watchdog_action("fast", 2_100, 5_000, true, false),
            WatchdogAction::None
        );
    }

    #[test]
    fn deadline_notice_is_real_and_sent_once() {
        assert_eq!(
            watchdog_action("fast", 5_999, 100, false, false),
            WatchdogAction::None
        );
        assert_eq!(
            watchdog_action("fast", 6_000, 100, false, false),
            WatchdogAction::Notice
        );
        assert_eq!(
            watchdog_action("fast", 9_000, 100, false, true),
            WatchdogAction::None
        );
        assert_eq!(
            watchdog_action("quality", 15_000, 100, false, false),
            WatchdogAction::Notice
        );
    }

    #[test]
    fn visible_content_suppresses_deadline_notice_but_allows_heartbeat() {
        assert_eq!(
            watchdog_action("quality", 16_000, 2_500, true, false),
            WatchdogAction::Heartbeat
        );
        assert_eq!(
            watchdog_action("quality", 16_000, 1_000, true, false),
            WatchdogAction::None
        );
    }
}
