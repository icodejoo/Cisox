//! 命令序号与 `exit_request` 握手（方案 §4）。
//!
//! "UI 正在退出时来了新热键"的竞态用序号握手仲裁，不用定时器：UI 判定无窗口且无活动后，
//! 发 `exit_request{last_seq}`（它处理过的最后一条命令序号）；宿主比较自己已投递的最大序号，
//! 相等就回 `exit_ok` 并转入 Exiting（此后命令全部入队），不等就回 `exit_denied`。
//! 宿主是唯一仲裁方，不存在"命令写进了一个已决定退出的 UI"。
//!
//! 这里只有纯数据结构，不碰管道与时钟，状态机本身在阶段 2 接入。

use crate::message::{HostMessage, UiMessage};

/// 宿主对一次退出请求的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitDecision {
    /// 同意：UI 已处理完宿主投递的全部命令。
    Ok,
    /// 拒绝：宿主还有 UI 没处理的命令（或请求的序号不合法）。
    Denied,
}

impl ExitDecision {
    /// 转成要回给 UI 的消息。
    ///
    /// ```rust
    /// use snow_host_protocol::message::HostMessage;
    /// use snow_host_protocol::seq::ExitDecision;
    /// assert_eq!(ExitDecision::Ok.reply(), HostMessage::ExitOk);
    /// assert_eq!(ExitDecision::Denied.reply(), HostMessage::ExitDenied);
    /// ```
    pub fn reply(self) -> HostMessage {
        match self {
            ExitDecision::Ok => HostMessage::ExitOk,
            ExitDecision::Denied => HostMessage::ExitDenied,
        }
    }
}

/// 宿主侧的序号分配与退出仲裁。
///
/// 序号从 1 开始单调递增；`0` 表示"一条都没投递过"。只有真正写进管道的命令才占用序号。
#[derive(Debug, Default, Clone)]
pub struct HostSeq {
    /// 已投递的最大序号。
    last_sent: u64,
}

impl HostSeq {
    /// 新建（尚未投递任何命令）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 为下一条要投递的命令分配序号。
    ///
    /// # 返回
    /// 新序号（从 1 起，严格递增）。
    ///
    /// ```rust
    /// use snow_host_protocol::seq::HostSeq;
    /// let mut seq = HostSeq::new();
    /// assert_eq!(seq.next(), 1);
    /// assert_eq!(seq.next(), 2);
    /// ```
    // 方法名沿用"取下一个序号"的直觉；它有副作用，不是迭代器，所以不实现 `Iterator`
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.last_sent += 1;
        self.last_sent
    }

    /// 已投递的最大序号（从未投递为 0）。
    pub fn last_sent(&self) -> u64 {
        self.last_sent
    }

    /// 裁决 UI 的退出请求。
    ///
    /// # 参数
    /// - `ui_last_seq`：UI 声称自己处理过的最后一条命令序号。
    ///
    /// # 返回
    /// 与已投递的最大序号相等才 [`ExitDecision::Ok`]；落后（还有命令没处理）或超前
    /// （UI 声称处理了没发过的序号，协议错误）都 [`ExitDecision::Denied`]。
    ///
    /// ```rust
    /// use snow_host_protocol::seq::{ExitDecision, HostSeq};
    /// let mut seq = HostSeq::new();
    /// seq.next();
    /// seq.next();
    /// assert_eq!(seq.arbitrate_exit(2), ExitDecision::Ok);
    /// assert_eq!(seq.arbitrate_exit(1), ExitDecision::Denied);
    /// ```
    pub fn arbitrate_exit(&self, ui_last_seq: u64) -> ExitDecision {
        if ui_last_seq == self.last_sent {
            ExitDecision::Ok
        } else {
            ExitDecision::Denied
        }
    }

    /// 直接处理 UI 消息里的 `exit_request`：不是退出请求返回 `None`。
    ///
    /// # 参数
    /// - `message`：UI 发来的消息。
    ///
    /// # 返回
    /// 退出请求对应的裁决。
    pub fn arbitrate(&self, message: &UiMessage) -> Option<ExitDecision> {
        match message {
            UiMessage::ExitRequest { last_seq } => Some(self.arbitrate_exit(*last_seq)),
            _ => None,
        }
    }
}

/// UI 对一条收到的命令序号的判断。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqCheck {
    /// 新命令（恰好是下一个序号）：处理并确认。
    Fresh,
    /// 重复（宿主因 ack 超时重发，序号不大于已处理的）：只重新确认，不要再处理。
    Duplicate,
    /// 序号跳号（中间有命令没收到）：仍然处理，但值得记日志。
    Gap,
}

/// UI 侧的序号跟踪：去重重发的命令，并记录退出请求要报的 `last_seq`。
#[derive(Debug, Default, Clone)]
pub struct UiSeq {
    /// 已处理的最后一条命令序号。
    last_processed: u64,
}

impl UiSeq {
    /// 新建（尚未处理任何命令）。
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条收到的命令序号并判断是否该处理。
    ///
    /// # 参数
    /// - `seq`：收到的命令序号。
    ///
    /// # 返回
    /// [`SeqCheck::Duplicate`] 时调用方不应再处理该命令；其余情形应处理，并已记入 `last_processed`。
    ///
    /// ```rust
    /// use snow_host_protocol::seq::{SeqCheck, UiSeq};
    /// let mut ui = UiSeq::new();
    /// assert_eq!(ui.accept(1), SeqCheck::Fresh);
    /// assert_eq!(ui.accept(1), SeqCheck::Duplicate);
    /// assert_eq!(ui.accept(3), SeqCheck::Gap);
    /// assert_eq!(ui.last_processed(), 3);
    /// ```
    pub fn accept(&mut self, seq: u64) -> SeqCheck {
        if seq <= self.last_processed {
            return SeqCheck::Duplicate;
        }
        let check = if seq == self.last_processed + 1 {
            SeqCheck::Fresh
        } else {
            SeqCheck::Gap
        };
        self.last_processed = seq;
        check
    }

    /// 已处理的最后一条命令序号（退出请求里的 `last_seq`；从未处理为 0）。
    pub fn last_processed(&self) -> u64 {
        self.last_processed
    }

    /// 生成退出请求消息。
    ///
    /// ```rust
    /// use snow_host_protocol::message::UiMessage;
    /// use snow_host_protocol::seq::UiSeq;
    /// let mut ui = UiSeq::new();
    /// ui.accept(1);
    /// assert_eq!(ui.exit_request(), UiMessage::ExitRequest { last_seq: 1 });
    /// ```
    pub fn exit_request(&self) -> UiMessage {
        UiMessage::ExitRequest {
            last_seq: self.last_processed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 序号从 1 起严格递增，初始为 0。
    #[test]
    fn host_seq_is_monotonic_from_one() {
        let mut seq = HostSeq::new();
        assert_eq!(seq.last_sent(), 0);
        let issued: Vec<u64> = (0..5).map(|_| seq.next()).collect();
        assert_eq!(issued, vec![1, 2, 3, 4, 5]);
        assert_eq!(seq.last_sent(), 5);
    }

    /// 退出仲裁：相等同意，落后拒绝，超前（协议错误）也拒绝；从未投递时 0 对 0 同意。
    #[test]
    fn arbitration_rules() {
        let mut seq = HostSeq::new();
        assert_eq!(seq.arbitrate_exit(0), ExitDecision::Ok);
        assert_eq!(seq.arbitrate_exit(1), ExitDecision::Denied);
        for _ in 0..3 {
            seq.next();
        }
        assert_eq!(seq.arbitrate_exit(3), ExitDecision::Ok);
        assert_eq!(seq.arbitrate_exit(2), ExitDecision::Denied);
        assert_eq!(seq.arbitrate_exit(4), ExitDecision::Denied);
        assert_eq!(seq.arbitrate_exit(0), ExitDecision::Denied);
    }

    /// 经消息入口仲裁：只认 exit_request，其余消息返回 None。
    #[test]
    fn arbitrate_from_message() {
        let mut seq = HostSeq::new();
        seq.next();
        assert_eq!(
            seq.arbitrate(&UiMessage::ExitRequest { last_seq: 1 }),
            Some(ExitDecision::Ok)
        );
        assert_eq!(
            seq.arbitrate(&UiMessage::ExitRequest { last_seq: 0 }),
            Some(ExitDecision::Denied)
        );
        assert_eq!(seq.arbitrate(&UiMessage::Ack { seq: 1 }), None);
    }

    /// 竞态：UI 请求退出的同时宿主又投递了新命令，仲裁必须拒绝，UI 处理完新命令后再请求才同意。
    #[test]
    fn exit_racing_with_new_command_is_denied() {
        let mut host = HostSeq::new();
        let mut ui = UiSeq::new();
        // 第 1 条命令已投递且被 UI 处理
        assert_eq!(ui.accept(host.next()), SeqCheck::Fresh);
        // UI 发出退出请求（此刻 last_seq = 1）
        let request = ui.exit_request();
        // 请求在途时，宿主又收到热键并投递第 2 条
        let in_flight = host.next();
        // 宿主收到请求：落后一条，拒绝
        assert_eq!(host.arbitrate(&request), Some(ExitDecision::Denied));
        assert_eq!(ExitDecision::Denied.reply(), HostMessage::ExitDenied);
        // UI 收到被拒绝前已读到第 2 条，处理后重新请求，这次同意
        assert_eq!(ui.accept(in_flight), SeqCheck::Fresh);
        assert_eq!(host.arbitrate(&ui.exit_request()), Some(ExitDecision::Ok));
    }

    /// UI 去重：重发的旧序号是 Duplicate，不会回退 last_processed；跳号是 Gap 但仍前进。
    #[test]
    fn ui_dedupes_resends() {
        let mut ui = UiSeq::new();
        assert_eq!(ui.last_processed(), 0);
        assert_eq!(ui.accept(1), SeqCheck::Fresh);
        assert_eq!(ui.accept(2), SeqCheck::Fresh);
        assert_eq!(ui.accept(2), SeqCheck::Duplicate);
        assert_eq!(ui.accept(1), SeqCheck::Duplicate);
        assert_eq!(ui.last_processed(), 2);
        assert_eq!(ui.accept(5), SeqCheck::Gap);
        assert_eq!(ui.last_processed(), 5);
        assert_eq!(ui.accept(4), SeqCheck::Duplicate);
        assert_eq!(ui.exit_request(), UiMessage::ExitRequest { last_seq: 5 });
    }
}
