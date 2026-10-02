//! 右下角浮窗的文本模型：PARTIAL / FINAL 序列如何变成显示内容，用户编辑后如何合并，复制什么。
//!
//! 取舍：可编辑文本区里只放“已落定 + 用户编辑过”的内容；未落定的 PARTIAL 单独显示在文本区之外
//! （灰色带下划线），因此后续识别永远不会覆盖用户的编辑——落定的 FINAL 只会追加到文本区**当前内容**的末尾。
//! 全部是纯逻辑，不依赖窗口，可离屏单测。

use super::status::Status;
use super::text::{join, sanitize};

/// 对文本区的待办改动，在下一次渲染时按文本区**当时的内容**折叠应用。
#[derive(Debug, Clone, PartialEq, Eq)]
enum TextOp {
    /// 清空。
    Clear,
    /// 追加一句落定的文字。
    AppendFinal(String),
}

/// 复制结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyState {
    /// 还没点过复制（或文字之后又变了）。
    Idle,
    /// 已复制。
    Copied,
    /// 复制失败及原因。
    Failed(String),
}

/// 浮窗模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayModel {
    /// 等待应用到文本区的改动。
    pending: Vec<TextOp>,
    /// 当前句的未落定文本。
    partial: String,
    /// 当前状态；没有时显示空。
    status: Option<Status>,
    /// 复制结果。
    copy: CopyState,
}

impl Default for OverlayModel {
    /// 空模型。
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            partial: String::new(),
            status: None,
            copy: CopyState::Idle,
        }
    }
}

impl OverlayModel {
    /// 开始新一轮：清空文本区、未落定文本、状态与复制结果。
    pub fn begin_round(&mut self) {
        self.pending.clear();
        self.pending.push(TextOp::Clear);
        self.partial.clear();
        self.status = None;
        self.copy = CopyState::Idle;
    }

    /// 更新未落定文本（整句替换）。
    ///
    /// # 参数
    /// - `text`：PARTIAL 原文。
    pub fn push_partial(&mut self, text: &str) {
        self.partial = sanitize(text);
        self.copy = CopyState::Idle;
    }

    /// 落定一句：排队追加到文本区末尾，并清掉未落定部分。
    ///
    /// # 参数
    /// - `text`：FINAL 原文。
    pub fn push_final(&mut self, text: &str) {
        self.partial.clear();
        let clean = sanitize(text);
        if !clean.is_empty() {
            self.pending.push(TextOp::AppendFinal(clean));
            self.copy = CopyState::Idle;
        }
    }

    /// 用已有的转写内容整体铺底（键入中途兜底到浮窗时用）：清空后放入已落定文本，并带上未落定部分。
    ///
    /// # 参数
    /// - `finals`：已落定文本。
    /// - `partial`：未落定文本。
    pub fn seed(&mut self, finals: &str, partial: &str) {
        self.pending.clear();
        self.pending.push(TextOp::Clear);
        if !finals.is_empty() {
            self.pending.push(TextOp::AppendFinal(finals.to_string()));
        }
        self.partial = sanitize(partial);
        self.copy = CopyState::Idle;
    }

    /// 设置状态。
    pub fn set_status(&mut self, status: Status) {
        self.status = Some(status);
    }

    /// 当前状态。
    pub fn status(&self) -> Option<&Status> {
        self.status.as_ref()
    }

    /// 未落定文本。
    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// 复制结果。
    pub fn copy_state(&self) -> &CopyState {
        &self.copy
    }

    /// 是否有待应用的文本改动。
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// 折叠待办改动，得到文本区应有的新内容；没有待办返回 `None`。
    ///
    /// # 参数
    /// - `current`：文本区此刻的内容（含用户编辑）。
    ///
    /// ```ignore
    /// let mut m = OverlayModel::default();
    /// m.push_final("世界");
    /// assert_eq!(m.take_text("你好（用户改过）").as_deref(), Some("你好（用户改过）世界"));
    /// ```
    pub fn take_text(&mut self, current: &str) -> Option<String> {
        if self.pending.is_empty() {
            return None;
        }
        let mut text = current.to_string();
        for op in self.pending.drain(..) {
            match op {
                TextOp::Clear => text.clear(),
                TextOp::AppendFinal(add) => text = join(&text, &add),
            }
        }
        Some(text)
    }

    /// 点击复制时要放进剪贴板的内容：文本区当前内容 + 尚未落定的部分（所见即所得）。
    ///
    /// # 参数
    /// - `current`：文本区此刻的内容。
    pub fn copy_payload(&self, current: &str) -> String {
        join(current.trim_end(), &self.partial)
    }

    /// 执行复制：内容为空时不动剪贴板；成功记为已复制，失败记下原因。
    ///
    /// # 参数
    /// - `current`：文本区此刻的内容。
    /// - `copier`：写剪贴板的函数。
    pub fn copy_now(&mut self, current: &str, copier: impl FnOnce(&str) -> Result<(), String>) {
        let payload = self.copy_payload(current);
        if payload.trim().is_empty() {
            return;
        }
        self.copy = match copier(&payload) {
            Ok(()) => CopyState::Copied,
            Err(reason) => CopyState::Failed(reason),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PARTIAL 不进入文本区，只在区外显示；FINAL 才追加，并清掉未落定部分。
    #[test]
    fn partial_stays_outside_until_final() {
        let mut m = OverlayModel::default();
        m.push_partial("你");
        m.push_partial("你好");
        assert_eq!(m.partial(), "你好");
        assert!(!m.has_pending());
        assert_eq!(m.take_text(""), None);
        m.push_final("你好");
        assert_eq!(m.partial(), "");
        assert_eq!(m.take_text("").as_deref(), Some("你好"));
        assert!(!m.has_pending());
    }

    /// 序列：多句 FINAL 依次追加，英文句之间补空格。
    #[test]
    fn finals_append_in_order() {
        let mut m = OverlayModel::default();
        m.push_final("hello there");
        m.push_final("how are you");
        assert_eq!(m.take_text("").as_deref(), Some("hello there how are you"));
        m.push_final("你好");
        m.push_final("世界");
        assert_eq!(m.take_text("").as_deref(), Some("你好世界"));
    }

    /// 用户编辑后的合并：新 FINAL 追加到文本区当前内容末尾，用户改过的部分原样保留。
    #[test]
    fn user_edits_are_never_overwritten() {
        let mut m = OverlayModel::default();
        m.push_final("你好");
        let shown = m.take_text("").unwrap();
        // 用户把已识别的“你好”改成了别的内容
        let edited = format!("{shown}（我改过）").replace("你好", "您好");
        m.push_partial("世");
        m.push_final("世界");
        assert_eq!(m.take_text(&edited).as_deref(), Some("您好（我改过）世界"));
        // 用户清空了文本区，后续 FINAL 只追加新内容
        m.push_final("再见");
        assert_eq!(m.take_text("").as_deref(), Some("再见"));
    }

    /// 空 FINAL 只清未落定部分，不产生改动。
    #[test]
    fn empty_final_only_clears_partial() {
        let mut m = OverlayModel::default();
        m.push_partial("abc");
        m.push_final("  ");
        assert_eq!(m.partial(), "");
        assert!(!m.has_pending());
    }

    /// 复制内容 = 文本区当前内容 + 未落定部分；失败记原因，空内容不动剪贴板。
    #[test]
    fn copy_payload_and_states() {
        let mut m = OverlayModel::default();
        m.push_partial("world");
        assert_eq!(m.copy_payload("hello"), "hello world");
        let mut copied = String::new();
        m.copy_now("hello", |t| {
            copied = t.to_string();
            Ok(())
        });
        assert_eq!(copied, "hello world");
        assert_eq!(m.copy_state(), &CopyState::Copied);
        // 文字之后又变了，复制标记失效
        m.push_partial("world!");
        assert_eq!(m.copy_state(), &CopyState::Idle);
        m.copy_now("hello", |_| Err("busy".into()));
        assert_eq!(m.copy_state(), &CopyState::Failed("busy".into()));

        let mut empty = OverlayModel::default();
        let mut called = false;
        empty.copy_now("  ", |_| {
            called = true;
            Ok(())
        });
        assert!(!called);
        assert_eq!(empty.copy_state(), &CopyState::Idle);
    }

    /// 新一轮清空：文本区被清、未落定部分与状态与复制标记复位；之前排队的追加作废。
    #[test]
    fn new_round_clears_everything() {
        let mut m = OverlayModel::default();
        m.push_final("旧内容");
        m.push_partial("残留");
        m.set_status(Status::Done);
        m.copy_now("旧内容", |_| Ok(()));
        m.begin_round();
        assert_eq!(m.partial(), "");
        assert!(m.status().is_none());
        assert_eq!(m.copy_state(), &CopyState::Idle);
        assert_eq!(m.take_text("旧内容（用户编辑过）").as_deref(), Some(""));
        m.push_final("新内容");
        assert_eq!(m.take_text("").as_deref(), Some("新内容"));
    }

    /// 兜底铺底：清空后放入已落定文本，未落定部分单独显示。
    #[test]
    fn seed_replaces_with_transcript() {
        let mut m = OverlayModel::default();
        m.seed("已经说的话", "还在说");
        assert_eq!(m.take_text("乱七八糟").as_deref(), Some("已经说的话"));
        assert_eq!(m.partial(), "还在说");
        assert_eq!(m.copy_payload("已经说的话"), "已经说的话还在说");
    }

    /// 状态可更新与读取。
    #[test]
    fn status_roundtrip() {
        let mut m = OverlayModel::default();
        assert!(m.status().is_none());
        m.set_status(Status::Loading);
        assert_eq!(m.status(), Some(&Status::Loading));
    }
}
