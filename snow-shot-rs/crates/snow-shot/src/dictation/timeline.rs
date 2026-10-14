//! 听写会话时间线：记录一个 `cisox-stt` 进程从拉起到识别就绪的各个时间点，供 info 日志定量。
//!
//! 只做计时与格式化，不碰进程与日志：调用方传入当前时刻，拿到毫秒数后自己写日志，
//! 所以可以离屏单测。基准有两个：进程拉起（`spawn`）和会话认领（`claim`，预拉起的 worker 在用户
//! 按键后才被认领；普通启动二者重合）。

use snow_stt_protocol::{Event, StartRequest};
use std::path::Path;
use std::time::{Duration, Instant};

/// worker 在 stderr 里输出、值得进 info 日志的行前缀：加载耗时、加载计划、`--stats` 统计。
pub const WORKER_INFO_PREFIXES: [&str; 3] = ["load:", "load_plan:", "stats:"];

/// 时间点名称：READY（进程已起来）。
pub const POINT_READY: &str = "ready";
/// 时间点名称：预拉起的 worker 被会话认领。
pub const POINT_CLAIMED: &str = "claimed";
/// 时间点名称：START 已发出。
pub const POINT_START: &str = "start";
/// 时间点名称：RECORDING（麦克风已开、已在录音）。
pub const POINT_RECORDING: &str = "recording";
/// 时间点名称：PONG（模型加载完、识别就绪）。
pub const POINT_PONG: &str = "pong";
/// 时间点名称：第一条识别文本。
pub const POINT_FIRST_TEXT: &str = "first_text";

/// 一次会话的时间线。
#[derive(Debug, Clone)]
pub struct SessionTimeline {
    /// 进程拉起时刻。
    spawned_at: Instant,
    /// 是否由预拉起路径创建。
    prespawned: bool,
    /// 被认领时刻（距拉起）；普通启动在拉起当下就是会话开始，视为 0。
    claimed: Option<Duration>,
    /// 收到 READY（距拉起）。
    ready: Option<Duration>,
    /// 发出 START（距拉起）。
    start: Option<Duration>,
    /// 收到 RECORDING（距拉起）。
    recording: Option<Duration>,
    /// 收到 PONG（距拉起）。
    pong: Option<Duration>,
    /// 收到第一条识别文本（距拉起）。
    first_text: Option<Duration>,
    /// 模型标识（模型目录名；系统语音为后端名）。
    model: Option<String>,
    /// worker 自己报的加载耗时（毫秒）。
    worker_load_ms: Option<u64>,
}

/// 摘要：各区间毫秒数，没走到的点为 `None`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// 模型标识。
    pub model: String,
    /// 是否经预拉起认领。
    pub prespawned: bool,
    /// 认领时刻距拉起的毫秒数（普通启动为 0）。
    pub claim_ms: u64,
    /// 拉起到 READY。
    pub spawn_to_ready_ms: Option<u64>,
    /// 拉起到发出 START。
    pub spawn_to_start_ms: Option<u64>,
    /// 拉起到 RECORDING。
    pub spawn_to_recording_ms: Option<u64>,
    /// 拉起到 PONG（识别就绪）。
    pub spawn_to_pong_ms: Option<u64>,
    /// 拉起到第一条识别文本。
    pub spawn_to_first_text_ms: Option<u64>,
    /// 认领到 RECORDING：用户感知的“按键到可说话”（认领前已到则为 0）。
    pub claim_to_recording_ms: Option<u64>,
    /// 认领到 PONG：用户感知的“按键到识别就绪”。
    pub claim_to_pong_ms: Option<u64>,
    /// worker 自报的模型加载耗时。
    pub worker_load_ms: Option<u64>,
}

/// 时长转毫秒（超出 `u64` 时饱和）。
fn as_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// 由 START 请求得到模型标识：本地模型取模型目录名，系统语音（无模型目录）取后端名。
///
/// # 参数
/// - `request`：START 请求。
///
/// # 返回
/// 单行标识文本。
pub fn model_label(request: &StartRequest) -> String {
    if request.model_dir.is_empty() {
        return format!("{:?}", request.backend).to_lowercase();
    }
    Path::new(&request.model_dir)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(&request.model_dir)
        .to_string()
}

/// worker 的 stderr 行是否应提升为 info 日志（加载耗时、加载计划、统计）。
///
/// # 参数
/// - `line`：一行 stderr 文本。
pub fn is_worker_info_line(line: &str) -> bool {
    let line = line.trim_start();
    WORKER_INFO_PREFIXES.iter().any(|p| line.starts_with(p))
}

/// 从 worker 的一行文本里取 `load_ms=<数字>`。
///
/// # 参数
/// - `line`：一行 stderr 文本。
///
/// # 返回
/// 加载耗时毫秒数；没有该字段或不是数字返回 `None`。
pub fn parse_load_ms(line: &str) -> Option<u64> {
    const KEY: &str = "load_ms=";
    let rest = &line[line.find(KEY)? + KEY.len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

impl SessionTimeline {
    /// 新建时间线，`spawned_at` 为进程拉起时刻。
    ///
    /// # 参数
    /// - `spawned_at`：进程拉起时刻。
    pub fn new(spawned_at: Instant) -> Self {
        Self {
            spawned_at,
            prespawned: false,
            claimed: None,
            ready: None,
            start: None,
            recording: None,
            pong: None,
            first_text: None,
            model: None,
            worker_load_ms: None,
        }
    }

    /// 标记这是预拉起的 worker（认领前不出摘要）。
    pub fn mark_prespawned(&mut self) {
        self.prespawned = true;
    }

    /// 预拉起的 worker 被认领。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    ///
    /// # 返回
    /// 认领时刻距拉起的毫秒数。
    pub fn mark_claimed(&mut self, now: Instant) -> u64 {
        let at = now.saturating_duration_since(self.spawned_at);
        self.claimed = Some(at);
        as_ms(at)
    }

    /// 会话摘要；预拉起但始终没人认领的 worker 不算一次会话，返回 `None`。
    pub fn report(&self) -> Option<Summary> {
        (!self.prespawned || self.claimed.is_some()).then(|| self.summary())
    }

    /// 记录 START 已发出。
    ///
    /// # 参数
    /// - `now`：当前时刻。
    /// - `request`：START 请求（用来取模型标识）。
    ///
    /// # 返回
    /// 首次记录时返回距拉起的毫秒数；重复发送返回 `None`。
    pub fn on_start_sent(&mut self, now: Instant, request: &StartRequest) -> Option<u64> {
        self.model = Some(model_label(request));
        Self::first(&mut self.start, self.spawned_at, now)
    }

    /// 记录 worker 事件到达（READY / RECORDING / PONG / 第一条文本）。
    ///
    /// # 参数
    /// - `now`：事件到达时刻。
    /// - `event`：协议事件。
    ///
    /// # 返回
    /// 首次出现某个时间点时返回 `(点名, 距拉起毫秒数)`；其余事件或重复出现返回 `None`。
    pub fn on_event(&mut self, now: Instant, event: &Event) -> Option<(&'static str, u64)> {
        let (point, slot) = match event {
            Event::Ready => (POINT_READY, &mut self.ready),
            Event::Recording => (POINT_RECORDING, &mut self.recording),
            Event::Pong => (POINT_PONG, &mut self.pong),
            Event::Partial(_) | Event::Final(_) => (POINT_FIRST_TEXT, &mut self.first_text),
            _ => return None,
        };
        Self::first(slot, self.spawned_at, now).map(|ms| (point, ms))
    }

    /// 记录 worker 的一行 stderr：带 `load_ms=` 的行取走加载耗时。
    ///
    /// # 参数
    /// - `line`：一行 stderr 文本。
    pub fn on_worker_line(&mut self, line: &str) {
        if let Some(ms) = parse_load_ms(line) {
            self.worker_load_ms.get_or_insert(ms);
        }
    }

    /// 只在槽位为空时写入，并返回距拉起的毫秒数。
    fn first(slot: &mut Option<Duration>, spawned_at: Instant, now: Instant) -> Option<u64> {
        if slot.is_some() {
            return None;
        }
        let at = now.saturating_duration_since(spawned_at);
        *slot = Some(at);
        Some(as_ms(at))
    }

    /// 汇总成摘要。
    fn summary(&self) -> Summary {
        let claim = self.claimed.unwrap_or(Duration::ZERO);
        let since_claim = |at: Option<Duration>| at.map(|d| as_ms(d.saturating_sub(claim)));
        Summary {
            model: self.model.clone().unwrap_or_else(|| "-".to_string()),
            prespawned: self.prespawned,
            claim_ms: as_ms(claim),
            spawn_to_ready_ms: self.ready.map(as_ms),
            spawn_to_start_ms: self.start.map(as_ms),
            spawn_to_recording_ms: self.recording.map(as_ms),
            spawn_to_pong_ms: self.pong.map(as_ms),
            spawn_to_first_text_ms: self.first_text.map(as_ms),
            claim_to_recording_ms: since_claim(self.recording),
            claim_to_pong_ms: since_claim(self.pong),
            worker_load_ms: self.worker_load_ms,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use snow_stt_protocol::BackendKind;

    /// 距基准 `ms` 毫秒的时刻。
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    /// 普通启动：认领时刻为 0，两套区间数字相同。
    #[test]
    fn normal_start_has_zero_claim() {
        let t0 = Instant::now();
        let mut tl = SessionTimeline::new(t0);
        assert_eq!(
            tl.on_event(at(t0, 40), &Event::Ready),
            Some((POINT_READY, 40))
        );
        let request = StartRequest {
            model_dir: "D:/m/stt/zipformer-x".into(),
            ..Default::default()
        };
        assert_eq!(tl.on_start_sent(at(t0, 41), &request), Some(41));
        tl.on_event(at(t0, 900), &Event::Recording);
        tl.on_event(at(t0, 2800), &Event::Pong);
        tl.on_worker_line("load: load_ms=1700 backend=local");
        let s = tl.summary();
        assert_eq!(s.model, "zipformer-x");
        assert!(!s.prespawned);
        assert_eq!(s.claim_ms, 0);
        assert_eq!(s.spawn_to_ready_ms, Some(40));
        assert_eq!(s.spawn_to_start_ms, Some(41));
        assert_eq!(s.spawn_to_recording_ms, Some(900));
        assert_eq!(s.claim_to_recording_ms, Some(900));
        assert_eq!(s.spawn_to_pong_ms, Some(2800));
        assert_eq!(s.claim_to_pong_ms, Some(2800));
        assert_eq!(s.worker_load_ms, Some(1700));
        assert!(tl.report().is_some());
    }

    /// 预拉起：认领后的区间扣掉认领前已过去的时间；认领前就到的点按 0 计。
    #[test]
    fn claim_offsets_are_subtracted() {
        let t0 = Instant::now();
        let mut tl = SessionTimeline::new(t0);
        tl.mark_prespawned();
        assert!(tl.report().is_none(), "没人认领不出摘要");
        tl.on_event(at(t0, 300), &Event::Recording);
        assert_eq!(tl.mark_claimed(at(t0, 500)), 500);
        tl.on_event(at(t0, 1300), &Event::Pong);
        let s = tl.summary();
        assert!(s.prespawned);
        assert_eq!(s.claim_ms, 500);
        assert_eq!(s.spawn_to_recording_ms, Some(300));
        assert_eq!(s.claim_to_recording_ms, Some(0), "认领前已在录音");
        assert_eq!(s.claim_to_pong_ms, Some(800));
        assert!(tl.report().is_some());
    }

    /// 每个时间点只记首次；无关事件不产生时间点；没走到的点是 `None`。
    #[test]
    fn points_are_recorded_once() {
        let t0 = Instant::now();
        let mut tl = SessionTimeline::new(t0);
        assert!(tl.on_event(at(t0, 10), &Event::Pong).is_some());
        assert!(tl.on_event(at(t0, 20), &Event::Pong).is_none());
        assert!(tl.on_event(at(t0, 20), &Event::Stopped).is_none());
        assert_eq!(
            tl.on_event(at(t0, 30), &Event::Partial("你".into())),
            Some((POINT_FIRST_TEXT, 30))
        );
        assert!(
            tl.on_event(at(t0, 40), &Event::Final("你好".into()))
                .is_none()
        );
        let s = tl.summary();
        assert_eq!(s.spawn_to_first_text_ms, Some(30));
        assert_eq!(s.spawn_to_ready_ms, None);
        assert_eq!(s.model, "-");
    }

    /// 模型标识：目录名；系统语音取后端名。
    #[test]
    fn model_label_variants() {
        let local = StartRequest {
            model_dir: "E:\\data\\stt\\x-asr-480ms".into(),
            ..Default::default()
        };
        assert_eq!(model_label(&local), "x-asr-480ms");
        let system = StartRequest {
            backend: BackendKind::System,
            ..Default::default()
        };
        assert_eq!(model_label(&system), "system");
    }

    /// worker 行：只认约定前缀；`load_ms` 取数字，缺失或非数字为 `None`。
    #[test]
    fn worker_line_parsing() {
        assert!(is_worker_info_line("load: load_ms=5 x=1"));
        assert!(is_worker_info_line(
            "  load_plan: encoder=cache model_type=zipformer"
        ));
        assert!(is_worker_info_line("stats: load_ms=7 chunks=3"));
        assert!(!is_worker_info_line("preroll: 环形缓存已满"));
        assert_eq!(parse_load_ms("stats: load_ms=812 chunks=3"), Some(812));
        assert_eq!(parse_load_ms("load: load_ms=0"), Some(0));
        assert_eq!(parse_load_ms("load_plan: encoder=cache"), None);
        assert_eq!(parse_load_ms("load: load_ms=abc"), None);
        let mut tl = SessionTimeline::new(Instant::now());
        tl.on_worker_line("stats: load_ms=1 chunks=3");
        tl.on_worker_line("load: load_ms=2");
        assert_eq!(tl.summary().worker_load_ms, Some(1), "只取第一次");
    }
}
