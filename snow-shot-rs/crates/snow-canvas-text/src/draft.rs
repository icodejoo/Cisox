//! 标注文本草稿与编辑缓冲区。
//!
//! 提供光标定位、多行选区、字符与字形团导航、撤销重做堆栈、
//! 以及与平台输入法（IME）对接的 UTF-8/UTF-16 偏移转换和预编辑标记管理。

use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// 撤销历史状态快照。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    /// 提交文本内容。
    text: String,
    /// 光标字节位置。
    cursor: usize,
    /// 选区锚点字节位置。
    anchor: usize,
}

/// 标注文本草稿缓冲区。
///
/// 封装文本编辑的核心逻辑，支持选区、光标、多步撤销重做与 IME 预编辑融合。
#[derive(Debug, Clone)]
pub struct TextDraft {
    /// 已提交文本（UTF-8 编码）。
    text: String,
    /// 当前光标位置（字节偏移，对齐到 UTF-8 字符边界）。
    cursor: usize,
    /// 选区锚点位置（字节偏移，与光标不同时存在选区）。
    anchor: usize,
    /// 输入法预编辑组合串（未上屏文本）。
    preedit_text: String,
    /// 预编辑串在已提交文本中的起始替换位置。
    preedit_start: usize,
    /// 预编辑串计划替换的已提交文本字节长度。
    preedit_replacement_len: usize,
    /// 预编辑串内部的光标位置（字节偏移）。
    preedit_cursor: usize,
    /// 撤销栈。
    undo_stack: Vec<Snapshot>,
    /// 重做栈。
    redo_stack: Vec<Snapshot>,
    /// 最大撤销深度。
    max_undo_depth: usize,
}

impl Default for TextDraft {
    /// 创建空的文本草稿。
    fn default() -> Self {
        Self::new()
    }
}

impl TextDraft {
    /// 默认最大撤销步数。
    pub const DEFAULT_MAX_UNDO: usize = 128;

    /// 创建一个空的文本草稿。
    ///
    /// # 返回
    /// 初始为空的草稿实例。
    ///
    /// # 示例
    /// ```
    /// use snow_canvas_text::TextDraft;
    /// let draft = TextDraft::new();
    /// assert_eq!(draft.text(), "");
    /// ```
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            anchor: 0,
            preedit_text: String::new(),
            preedit_start: 0,
            preedit_replacement_len: 0,
            preedit_cursor: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_undo_depth: Self::DEFAULT_MAX_UNDO,
        }
    }

    /// 使用初始文本创建文本草稿，并将光标置于末尾。
    ///
    /// # 参数
    /// - `text`：初始文本内容。
    ///
    /// # 返回
    /// 包含初始文本的草稿实例。
    ///
    /// # 示例
    /// ```
    /// use snow_canvas_text::TextDraft;
    /// let draft = TextDraft::with_text("Hello");
    /// assert_eq!(draft.text(), "Hello");
    /// assert_eq!(draft.cursor(), 5);
    /// ```
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let len = text.len();
        Self {
            text,
            cursor: len,
            anchor: len,
            preedit_text: String::new(),
            preedit_start: 0,
            preedit_replacement_len: 0,
            preedit_cursor: 0,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            max_undo_depth: Self::DEFAULT_MAX_UNDO,
        }
    }

    /// 获取已提交的底层文本。
    ///
    /// # 返回
    /// 文本切片。
    pub fn text(&self) -> &str {
        &self.text
    }

    /// 获取用于显示的合成文本（包含预编辑文本）。
    ///
    /// # 返回
    /// 合成后的显示文本。
    ///
    /// # 示例
    /// ```
    /// use snow_canvas_text::TextDraft;
    /// let draft = TextDraft::with_text("abc");
    /// assert_eq!(draft.display_text(), "abc");
    /// ```
    pub fn display_text(&self) -> String {
        if self.preedit_text.is_empty() {
            return self.text.clone();
        }
        let start = self.preedit_start.min(self.text.len());
        let replace_len = self.preedit_replacement_len.min(self.text.len() - start);
        let mut result = String::with_capacity(self.text.len() + self.preedit_text.len());
        result.push_str(&self.text[..start]);
        result.push_str(&self.preedit_text);
        result.push_str(&self.text[start + replace_len..]);
        result
    }

    /// 获取当前光标字节偏移。
    ///
    /// # 返回
    /// 光标在已提交文本中的字节偏移。
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// 获取当前选区锚点字节偏移。
    ///
    /// # 返回
    /// 锚点在已提交文本中的字节偏移。
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// 获取显示合成文本下的光标字节偏移。
    ///
    /// # 返回
    /// 在 `display_text` 字符串中的字节偏移。
    pub fn display_cursor(&self) -> usize {
        if self.preedit_text.is_empty() {
            return self.cursor;
        }
        let start = self.preedit_start.min(self.text.len());
        (start + self.preedit_cursor).min(self.display_text().len())
    }

    /// 获取显示合成文本下的选区锚点字节偏移。
    ///
    /// # 返回
    /// 在 `display_text` 字符串中的字节偏移。
    pub fn display_anchor(&self) -> usize {
        if self.preedit_text.is_empty() {
            return self.anchor;
        }
        self.display_cursor()
    }

    /// 获取当前选区范围（以较小索引为起点，较大为终点）。
    ///
    /// # 返回
    /// 规范化选区字节范围。
    pub fn selection_range(&self) -> Range<usize> {
        let start = self.cursor.min(self.anchor).min(self.text.len());
        let end = self.cursor.max(self.anchor).min(self.text.len());
        start..end
    }

    /// 是否存在有效选区（光标与锚点不同）。
    ///
    /// # 返回
    /// 若有选中文本返回 `true`。
    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    /// 获取当前选中的文本切片。
    ///
    /// # 返回
    /// 选中的文本字符串切片。
    pub fn selected_text(&self) -> &str {
        if !self.has_selection() {
            return "";
        }
        let range = self.selection_range();
        &self.text[range]
    }

    /// 获取当前预编辑组合文本。
    ///
    /// # 返回
    /// 预编辑文本切片。
    pub fn preedit_text(&self) -> &str {
        &self.preedit_text
    }

    /// 是否存在活跃的输入法预编辑组合。
    ///
    /// # 返回
    /// 若存在返回 `true`。
    pub fn has_preedit(&self) -> bool {
        !self.preedit_text.is_empty()
    }

    /// 获取当前预编辑在已提交文本中的字节区间。
    ///
    /// # 返回
    /// 若存在预编辑则返回其范围。
    pub fn marked_range(&self) -> Option<Range<usize>> {
        if self.preedit_text.is_empty() {
            None
        } else {
            let start = self.preedit_start.min(self.text.len());
            Some(start..start + self.preedit_text.len())
        }
    }

    /// 设定光标位置。
    ///
    /// # 参数
    /// - `position`：目标字节偏移。
    /// - `keep_selection`：是否保持当前锚点形成选区。
    ///
    /// # 返回
    /// 若位置发生变更返回 `true`。
    pub fn set_cursor(&mut self, position: usize, keep_selection: bool) -> bool {
        let bounded = self.snap_to_char_boundary(position.min(self.text.len()));
        let new_anchor = if keep_selection { self.anchor } else { bounded };
        if self.cursor == bounded && self.anchor == new_anchor {
            return false;
        }
        self.cursor = bounded;
        self.anchor = new_anchor;
        true
    }

    /// 全选文本。
    ///
    /// # 返回
    /// 若选区发生改变返回 `true`。
    pub fn select_all(&mut self) -> bool {
        self.clear_preedit();
        let len = self.text.len();
        if self.anchor == 0 && self.cursor == len {
            return false;
        }
        self.anchor = 0;
        self.cursor = len;
        true
    }

    /// 光标向左移动一个字形团（Grapheme Cluster）。
    ///
    /// # 参数
    /// - `keep_selection`：是否保持锚点扩展选区。
    ///
    /// # 返回
    /// 若光标移动成功返回 `true`。
    pub fn move_left(&mut self, keep_selection: bool) -> bool {
        self.clear_preedit();
        if !keep_selection && self.has_selection() {
            let start = self.selection_range().start;
            return self.set_cursor(start, false);
        }
        if self.cursor == 0 {
            return false;
        }
        let prev = self.previous_grapheme_boundary(self.cursor);
        self.set_cursor(prev, keep_selection)
    }

    /// 光标向右移动一个字形团（Grapheme Cluster）。
    ///
    /// # 参数
    /// - `keep_selection`：是否保持锚点扩展选区。
    ///
    /// # 返回
    /// 若光标移动成功返回 `true`。
    pub fn move_right(&mut self, keep_selection: bool) -> bool {
        self.clear_preedit();
        if !keep_selection && self.has_selection() {
            let end = self.selection_range().end;
            return self.set_cursor(end, false);
        }
        if self.cursor >= self.text.len() {
            return false;
        }
        let next = self.next_grapheme_boundary(self.cursor);
        self.set_cursor(next, keep_selection)
    }

    /// 移动光标至文本开头。
    ///
    /// # 参数
    /// - `keep_selection`：是否保持选区。
    ///
    /// # 返回
    /// 若光标发生变动返回 `true`。
    pub fn move_home(&mut self, keep_selection: bool) -> bool {
        self.clear_preedit();
        let line_start = self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
        self.set_cursor(line_start, keep_selection)
    }

    /// 移动光标至文本结尾。
    ///
    /// # 参数
    /// - `keep_selection`：是否保持选区。
    ///
    /// # 返回
    /// 若光标发生变动返回 `true`。
    pub fn move_end(&mut self, keep_selection: bool) -> bool {
        self.clear_preedit();
        let line_end = self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i);
        self.set_cursor(line_end, keep_selection)
    }

    /// 插入文本，替换当前选区（若有）。
    ///
    /// # 参数
    /// - `inserted`：插入的文本字符串。
    ///
    /// # 返回
    /// 若文本发生变化返回 `true`。
    pub fn insert_text(&mut self, inserted: &str) -> bool {
        self.clear_preedit();
        let normalized = normalize_newlines(inserted);
        let range = self.selection_range();
        if range.is_empty() && normalized.is_empty() {
            return false;
        }
        self.record_undo();
        self.text.replace_range(range.clone(), &normalized);
        let next_pos = range.start + normalized.len();
        self.cursor = next_pos;
        self.anchor = next_pos;
        true
    }

    /// 向后删除一个字符/字形团（Backspace）。
    ///
    /// # 返回
    /// 若发生删除变动返回 `true`。
    pub fn delete_backward(&mut self) -> bool {
        self.clear_preedit();
        if self.has_selection() {
            return self.insert_text("");
        }
        if self.cursor == 0 {
            return false;
        }
        let prev = self.previous_grapheme_boundary(self.cursor);
        self.record_undo();
        self.text.replace_range(prev..self.cursor, "");
        self.cursor = prev;
        self.anchor = prev;
        true
    }

    /// 向前删除一个字符/字形团（Delete）。
    ///
    /// # 返回
    /// 若发生删除变动返回 `true`。
    pub fn delete_forward(&mut self) -> bool {
        self.clear_preedit();
        if self.has_selection() {
            return self.insert_text("");
        }
        if self.cursor >= self.text.len() {
            return false;
        }
        let next = self.next_grapheme_boundary(self.cursor);
        self.record_undo();
        self.text.replace_range(self.cursor..next, "");
        self.anchor = self.cursor;
        true
    }

    /// 清空所有内容。
    ///
    /// # 返回
    /// 若内容发生清空变动返回 `true`。
    pub fn clear(&mut self) -> bool {
        self.clear_preedit();
        if self.text.is_empty() && self.cursor == 0 && self.anchor == 0 {
            return false;
        }
        self.record_undo();
        self.text.clear();
        self.cursor = 0;
        self.anchor = 0;
        true
    }

    /// 清空输入法预编辑状态。
    ///
    /// # 返回
    /// 若之前存在预编辑状态返回 `true`。
    pub fn clear_preedit(&mut self) -> bool {
        if self.preedit_text.is_empty()
            && self.preedit_start == 0
            && self.preedit_replacement_len == 0
            && self.preedit_cursor == 0
        {
            return false;
        }
        self.preedit_text.clear();
        self.preedit_start = 0;
        self.preedit_replacement_len = 0;
        self.preedit_cursor = 0;
        true
    }

    /// 执行撤销操作。
    ///
    /// # 返回
    /// 若撤销成功返回 `true`。
    pub fn undo(&mut self) -> bool {
        self.clear_preedit();
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.current_snapshot());
        self.apply_snapshot(snapshot);
        true
    }

    /// 执行重做操作。
    ///
    /// # 返回
    /// 若重做成功返回 `true`。
    pub fn redo(&mut self) -> bool {
        self.clear_preedit();
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.current_snapshot());
        self.apply_snapshot(snapshot);
        true
    }

    /// 是否可以执行撤销。
    ///
    /// # 返回
    /// 撤销栈非空返回 `true`。
    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    /// 是否可以执行重做。
    ///
    /// # 返回
    /// 重做栈非空返回 `true`。
    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// UTF-8 字节偏移转 UTF-16 单元偏移。
    ///
    /// # 参数
    /// - `offset_utf8`：UTF-8 字节偏移。
    ///
    /// # 返回
    /// 对应的 UTF-16 单元偏移。
    pub fn to_utf16(&self, offset_utf8: usize) -> usize {
        let bounded = offset_utf8.min(self.text.len());
        self.text[..bounded].chars().map(char::len_utf16).sum()
    }

    /// UTF-16 单元偏移转 UTF-8 字节偏移。
    ///
    /// # 参数
    /// - `offset_utf16`：UTF-16 单元偏移。
    ///
    /// # 返回
    /// 对应的 UTF-8 字节偏移。
    pub fn from_utf16(&self, offset_utf16: usize) -> usize {
        let mut u16_count = 0;
        let mut u8_offset = 0;
        for ch in self.text.chars() {
            if u16_count >= offset_utf16 {
                break;
            }
            u16_count += ch.len_utf16();
            u8_offset += ch.len_utf8();
        }
        u8_offset
    }

    /// 区间 UTF-8 转 UTF-16。
    ///
    /// # 参数
    /// - `range`：UTF-8 字节区间。
    ///
    /// # 返回
    /// UTF-16 单元区间。
    pub fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.to_utf16(range.start)..self.to_utf16(range.end)
    }

    /// 区间 UTF-16 转 UTF-8。
    ///
    /// # 参数
    /// - `range`：UTF-16 单元区间。
    ///
    /// # 返回
    /// UTF-8 字节区间。
    pub fn range_from_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.from_utf16(range.start)..self.from_utf16(range.end)
    }

    /// 显示文本（含预编辑）中的 UTF-8 字节偏移转 UTF-16 单元偏移。
    ///
    /// 与系统输入法交互的所有偏移都基于显示文本；无预编辑时等同 [`TextDraft::to_utf16`]。
    ///
    /// # 参数
    /// - `offset_utf8`：显示文本内的字节偏移，超界按末尾处理。
    ///
    /// ```
    /// use snow_canvas_text::TextDraft;
    /// let mut d = TextDraft::with_text("ab");
    /// d.ime_replace_and_mark(None, "你好", None);
    /// // 显示文本 "ab你好"：预编辑之后的偏移必须计入预编辑的 UTF-16 长度
    /// assert_eq!(d.display_to_utf16("ab你好".len()), 4);
    /// ```
    pub fn display_to_utf16(&self, offset_utf8: usize) -> usize {
        let display = self.display_text();
        let bounded = floor_char_boundary(&display, offset_utf8);
        display[..bounded].chars().map(char::len_utf16).sum()
    }

    /// 显示文本中的 UTF-16 单元偏移转 UTF-8 字节偏移（落在代理对中间时向后取整）。
    ///
    /// # 参数
    /// - `offset_utf16`：显示文本内的 UTF-16 偏移。
    ///
    /// ```
    /// use snow_canvas_text::TextDraft;
    /// let mut d = TextDraft::with_text("a");
    /// d.ime_replace_and_mark(None, "🚀", None);
    /// assert_eq!(d.display_from_utf16(3), "a🚀".len());
    /// ```
    pub fn display_from_utf16(&self, offset_utf16: usize) -> usize {
        offset_from_utf16_in_str(&self.display_text(), offset_utf16)
    }

    /// 预编辑在显示文本中的字节区间；无预编辑返回 `None`。
    pub fn display_marked_range(&self) -> Option<Range<usize>> {
        self.marked_range()
    }

    /// 显示文本坐标下的选区：有预编辑时为预编辑内部的插入点（折叠区间），否则为常规选区。
    pub fn display_selection_range(&self) -> Range<usize> {
        if self.has_preedit() {
            let caret = self.display_cursor();
            caret..caret
        } else {
            self.selection_range()
        }
    }

    /// 把系统输入法给出的 UTF-16 区间（显示文本坐标）换算成已提交文本的字节区间。
    ///
    /// 区间端点落在预编辑内部时，起点收敛到被替换区起点、终点收敛到被替换区终点；
    /// 区间反向时自动交换；结果一定落在字符边界上。
    ///
    /// # 参数
    /// - `range`：显示文本坐标的 UTF-16 区间。
    ///
    /// ```
    /// use snow_canvas_text::TextDraft;
    /// let mut d = TextDraft::with_text("ab");
    /// d.ime_replace_and_mark(None, "ni", None);
    /// // 显示文本 "abni"，预编辑 2..4；恰好覆盖预编辑的区间对应提交文本的 2..2
    /// assert_eq!(d.committed_range_from_display_utf16(&(2..4)), 2..2);
    /// ```
    pub fn committed_range_from_display_utf16(&self, range: &Range<usize>) -> Range<usize> {
        let display = self.display_text();
        let s = offset_from_utf16_in_str(&display, range.start.min(range.end));
        let e = offset_from_utf16_in_str(&display, range.start.max(range.end));
        if !self.has_preedit() {
            return s..e;
        }
        let start = self.preedit_start.min(self.text.len());
        let preedit_len = self.preedit_text.len();
        let replaced = self.preedit_replacement_len.min(self.text.len() - start);
        let after = |p: usize| p - preedit_len + replaced;
        let map_start = if s <= start {
            s
        } else if s >= start + preedit_len {
            after(s)
        } else {
            start
        };
        let map_end = if e <= start {
            e
        } else if e >= start + preedit_len {
            after(e)
        } else {
            start + replaced
        };
        map_start..map_end.max(map_start)
    }

    /// 处理输入法上屏或直接替换文本（EntityInputHandler::replace_text_in_range）。
    ///
    /// # 参数
    /// - `range_utf16`：可选的替换目标区间（UTF-16，显示文本坐标）。
    /// - `text`：提交替换的文本。
    pub fn ime_replace_text(&mut self, range_utf16: Option<Range<usize>>, text: &str) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.committed_range_from_display_utf16(r))
            .or_else(|| {
                if !self.preedit_text.is_empty() {
                    let start = self.preedit_start.min(self.text.len());
                    let len = self.preedit_replacement_len.min(self.text.len() - start);
                    Some(start..start + len)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.selection_range());

        let range = self.clamp_range(range);
        let normalized = normalize_newlines(text);
        if self.text[range.clone()] != normalized {
            self.record_undo();
            self.text.replace_range(range.clone(), &normalized);
        }
        let next_pos = range.start + normalized.len();
        self.cursor = next_pos;
        self.anchor = next_pos;
        self.clear_preedit();
    }

    /// 处理输入法预编辑组合变化（EntityInputHandler::replace_and_mark_text_in_range）。
    ///
    /// # 参数
    /// - `range_utf16`：可选的替换目标区间（UTF-16）。
    /// - `text`：组合串文本（例如拼音 "nihao"）。
    /// - `new_sel_utf16`：组合内部的新选区/光标区间（UTF-16）。
    pub fn ime_replace_and_mark(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        new_sel_utf16: Option<Range<usize>>,
    ) {
        let target_range = range_utf16
            .as_ref()
            .map(|r| self.committed_range_from_display_utf16(r))
            .or_else(|| {
                if !self.preedit_text.is_empty() {
                    let start = self.preedit_start.min(self.text.len());
                    let len = self.preedit_replacement_len.min(self.text.len() - start);
                    Some(start..start + len)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| self.selection_range());

        if text.is_empty() {
            self.clear_preedit();
            return;
        }

        self.preedit_start = target_range.start;
        self.preedit_replacement_len = target_range.len();
        self.preedit_text = text.to_string();

        if let Some(sel) = new_sel_utf16 {
            let internal_u8 = offset_from_utf16_in_str(text, sel.end);
            self.preedit_cursor = internal_u8.min(text.len());
        } else {
            self.preedit_cursor = text.len();
        }
    }

    /// 撤销并结束当前预编辑标记（EntityInputHandler::unmark_text）。
    pub fn ime_unmark(&mut self) {
        self.clear_preedit();
    }

    /// 将字节索引吸附到合法字符起始边界。
    fn snap_to_char_boundary(&self, index: usize) -> usize {
        let mut idx = index.min(self.text.len());
        while idx > 0 && !self.text.is_char_boundary(idx) {
            idx -= 1;
        }
        idx
    }

    /// 计算前一个字形团边界。
    fn previous_grapheme_boundary(&self, position: usize) -> usize {
        let pos = position.min(self.text.len());
        if pos == 0 {
            return 0;
        }
        self.text[..pos]
            .grapheme_indices(true)
            .next_back()
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    /// 计算后一个字形团边界。
    fn next_grapheme_boundary(&self, position: usize) -> usize {
        let pos = position.min(self.text.len());
        if pos >= self.text.len() {
            return self.text.len();
        }
        self.text[pos..]
            .grapheme_indices(true)
            .nth(1)
            .map(|(idx, _)| pos + idx)
            .unwrap_or(self.text.len())
    }

    /// 记录当前状态到撤销栈，并清空重做栈。
    fn record_undo(&mut self) {
        self.undo_stack.push(self.current_snapshot());
        if self.undo_stack.len() > self.max_undo_depth {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }

    /// 获取当前文本与光标快照。
    fn current_snapshot(&self) -> Snapshot {
        Snapshot {
            text: self.text.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    /// 恢复指定快照。
    fn apply_snapshot(&mut self, snapshot: Snapshot) {
        self.text = snapshot.text;
        self.cursor = self.snap_to_char_boundary(snapshot.cursor.min(self.text.len()));
        self.anchor = self.snap_to_char_boundary(snapshot.anchor.min(self.text.len()));
    }
}

impl TextDraft {
    /// 把字节区间夹进提交文本并保证端点落在字符边界上。
    fn clamp_range(&self, range: Range<usize>) -> Range<usize> {
        let start = floor_char_boundary(&self.text, range.start);
        let end = floor_char_boundary(&self.text, range.end).max(start);
        start..end
    }
}

/// 不超过 `index` 的最大字符边界（超界取末尾）。
fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut i = index.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// 规范化换行符（CRLF/CR -> LF）。
fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// 计算指定字符串内部 UTF-16 对应的 UTF-8 字节偏移。
fn offset_from_utf16_in_str(text: &str, off_utf16: usize) -> usize {
    let mut u16_count = 0;
    let mut u8_offset = 0;
    for ch in text.chars() {
        if u16_count >= off_utf16 {
            break;
        }
        u16_count += ch.len_utf16();
        u8_offset += ch.len_utf8();
    }
    u8_offset
}
