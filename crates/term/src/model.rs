//! 终端模型：对 `alacritty_terminal` 的轻量封装。
//!
//! 职责：
//! - 持有 `alacritty_terminal::Term` 终端状态机（VT 解析、网格、滚动缓冲、选区）。
//! - 通过 `vte::Parser` + 自定义 `Perform` 适配层把字节流喂给 Term 的 `Handler` 实现。
//! - 向上层暴露尺寸管理、内容快照（`RenderableContent`）与终端事件（标题 / 剪贴板 / 铃声）。
//!
//! 不负责绘制（见 `renderer`）。

use std::sync::mpsc::{self, Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, RenderableContent, Term};
use alacritty_terminal::vte::ansi::{Attr, Color, Handler, NamedColor, StandardCharset};
use alacritty_terminal::vte::{Params, Parser, Perform};

/// 终端网格尺寸（列 × 行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: usize,
    pub rows: usize,
}

impl TerminalSize {
    pub fn new(columns: usize, rows: usize) -> Self {
        Self { columns, rows }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// 终端向上层发出的应用级事件。
#[derive(Debug, Clone)]
pub enum TerminalEvent {
    /// 窗口标题变化。
    Title(String),
    /// 重置窗口标题。
    ResetTitle,
    /// OSC52 剪贴板写入请求（携带 Base64 编码的文本）。
    ClipboardStore(u8, String),
    /// 终端请求写入 PTY（应用键 / 颜色查询等）。
    PtyWrite(String),
    /// 终端铃声。
    Bell,
    /// 新内容可用（提示渲染线程唤醒）。
    Wakeup,
    /// 网格变化可能影响鼠标光标形状。
    MouseCursorDirty,
}

/// 事件监听器：把 `alacritty_terminal` 的 `Event` 映射为 `TerminalEvent` 并送入 channel。
#[derive(Clone)]
pub struct EventSink {
    tx: Sender<TerminalEvent>,
}

impl EventSink {
    pub fn new(tx: Sender<TerminalEvent>) -> Self {
        Self { tx }
    }
}

impl EventListener for EventSink {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Title(title) => Some(TerminalEvent::Title(title)),
            Event::ResetTitle => Some(TerminalEvent::ResetTitle),
            Event::ClipboardStore(_, text) => Some(TerminalEvent::ClipboardStore(1, text)),
            Event::PtyWrite(text) => Some(TerminalEvent::PtyWrite(text)),
            Event::Bell => Some(TerminalEvent::Bell),
            Event::Wakeup => Some(TerminalEvent::Wakeup),
            Event::MouseCursorDirty => Some(TerminalEvent::MouseCursorDirty),
            _ => None,
        };
        if let Some(ev) = mapped {
            let _ = self.tx.send(ev);
        }
    }
}

/// `vte::Perform` 适配层：把 `Parser` 的 8 个入口转发到 `Term` 的 `Handler` 实现。
///
/// 设计参照 alacritty 的 `vt/perform.rs`（MIT），但裁剪到终端渲染所需的子集。
struct Performer<'a, T: EventListener> {
    term: &'a mut Term<T>,
}

impl<'a, T: EventListener> Perform for Performer<'a, T> {
    fn print(&mut self, c: char) {
        self.term.input(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.term.bell(),
            0x08 => self.term.backspace(),
            0x09 => self.term.put_tab(1),
            0x0a..=0x0c => self.term.linefeed(),
            0x0d => self.term.carriage_return(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let private = intermediates.last() == Some(&b'?');
        let mode_byte = intermediates.last().copied();

        match action {
            '@' => {
                let n = count(params, 0);
                self.term.insert_blank(n);
            }
            'A' => {
                let n = count(params, 0);
                self.term.move_up(n);
            }
            'B' => {
                let n = count(params, 0);
                self.term.move_down(n);
            }
            'C' => {
                let n = count(params, 0);
                self.term.move_forward(n);
            }
            'D' => {
                let n = count(params, 0);
                self.term.move_backward(n);
            }
            'E' => {
                let n = count(params, 0);
                self.term.move_down_and_cr(n);
            }
            'F' => {
                let n = count(params, 0);
                self.term.move_up_and_cr(n);
            }
            'G' => {
                let n = count(params, 0);
                self.term.goto_col(n.saturating_sub(1));
            }
            'H' | 'f' => {
                let line = row(params);
                let col = col(params);
                self.term.goto(line as i32, col);
            }
            'I' => {
                let n = count(params, 0);
                self.term.move_forward_tabs(n as u16);
            }
            'J' => {
                let mode = param_or(params, 0, 0);
                let clear_mode = match mode {
                    0 => alacritty_terminal::vte::ansi::ClearMode::Below,
                    1 => alacritty_terminal::vte::ansi::ClearMode::Above,
                    2 => alacritty_terminal::vte::ansi::ClearMode::All,
                    3 => alacritty_terminal::vte::ansi::ClearMode::Saved,
                    _ => alacritty_terminal::vte::ansi::ClearMode::Below,
                };
                self.term.clear_screen(clear_mode);
            }
            'K' => {
                let mode = param_or(params, 0, 0);
                let clear_mode = match mode {
                    1 => alacritty_terminal::vte::ansi::LineClearMode::Left,
                    2 => alacritty_terminal::vte::ansi::LineClearMode::All,
                    _ => alacritty_terminal::vte::ansi::LineClearMode::Right,
                };
                self.term.clear_line(clear_mode);
            }
            'L' => {
                let n = count(params, 0);
                self.term.insert_blank_lines(n);
            }
            'M' => {
                let n = count(params, 0);
                self.term.delete_lines(n);
            }
            'P' => {
                let n = count(params, 0);
                self.term.delete_chars(n);
            }
            'S' => {
                let n = count(params, 0);
                self.term.scroll_up(n);
            }
            'T'
                // CSI T 仅单参数时是 SD（向下滚动）；多参数为鼠标滚轮序列，忽略。
                if params.len() == 1 => {
                    let n = count(params, 0);
                    self.term.scroll_down(n);
                }
            'X' => {
                let n = count(params, 0);
                self.term.erase_chars(n);
            }
            'Z' => {
                let n = count(params, 0);
                self.term.move_backward_tabs(n as u16);
            }
            '`' => {
                let n = count(params, 0);
                self.term.goto_col(n.saturating_sub(1));
            }
            'b' => {
                let n = count(params, 0);
                self.repeat_char(n);
            }
            'c' => {
                self.term.identify_terminal(None);
            }
            'd' => {
                let n = count(params, 0);
                self.term.goto_line(n.saturating_sub(1) as i32);
            }
            'g' => {
                let mode = param_or(params, 0, 0);
                let clear_mode = match mode {
                    3 => alacritty_terminal::vte::ansi::TabulationClearMode::All,
                    _ => alacritty_terminal::vte::ansi::TabulationClearMode::Current,
                };
                self.term.clear_tabs(clear_mode);
            }
            'h' => {
                let mode = param_or(params, 0, 0);
                if private {
                    self.term.set_private_mode(private_mode(mode));
                } else {
                    self.term.set_mode(public_mode(mode));
                }
            }
            'l' => {
                let mode = param_or(params, 0, 0);
                if private {
                    self.term.unset_private_mode(private_mode(mode));
                } else {
                    self.term.unset_mode(public_mode(mode));
                }
            }
            'm' => self.sgr(params),
            'n' => {
                let n = param_or(params, 0, 0);
                self.term.device_status(n as usize);
            }
            'p' => {
                let mode = param_or(params, 0, 0);
                if private {
                    self.term.report_private_mode(private_mode(mode));
                } else {
                    self.term.report_mode(public_mode(mode));
                }
            }
            'q'
                // DECSCUSR：CSI Ps SP q。shape = Ps - 1，0/1/2 为默认区块，3/4 下划线。
                if mode_byte == Some(b' ') => {
                    let n = param_or(params, 0, 0);
                    let shape = match n {
                        0..=2 => alacritty_terminal::vte::ansi::CursorShape::Block,
                        3 | 4 => alacritty_terminal::vte::ansi::CursorShape::Underline,
                        _ => alacritty_terminal::vte::ansi::CursorShape::Block,
                    };
                    self.term.set_cursor_shape(shape);
                }
            'r' => {
                let top = param(params, 0).map(|v| v as usize).unwrap_or(1);
                let bottom = param(params, 1).map(|v| v as usize);
                self.term.set_scrolling_region(top.saturating_sub(1), bottom);
            }
            's' => self.term.save_cursor_position(),
            'u' => self.term.restore_cursor_position(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'7' if intermediates.is_empty() => self.term.save_cursor_position(),
            b'8' if intermediates.is_empty() => self.term.restore_cursor_position(),
            b'D' if intermediates.is_empty() => self.term.linefeed(),
            b'E' if intermediates.is_empty() => self.term.newline(),
            b'M' if intermediates.is_empty() => self.term.reverse_index(),
            b'H' if intermediates.is_empty() => self.term.set_horizontal_tabstop(),
            b'Z' if intermediates.is_empty() => self.term.identify_terminal(None),
            b'c' if intermediates.is_empty() => self.term.reset_state(),
            b'=' => self.term.set_keypad_application_mode(),
            b'>' => self.term.unset_keypad_application_mode(),
            b'8' if intermediates == [b'#'] => self.term.decaln(),
            b'B' if intermediates == [b'('] => {
                self.term.configure_charset(
                    alacritty_terminal::vte::ansi::CharsetIndex::G0,
                    StandardCharset::Ascii,
                );
            }
            b'0' if intermediates == [b'('] => {
                self.term.configure_charset(
                    alacritty_terminal::vte::ansi::CharsetIndex::G0,
                    StandardCharset::SpecialCharacterAndLineDrawing,
                );
            }
            b'B' if intermediates == [b')'] => {
                self.term.configure_charset(
                    alacritty_terminal::vte::ansi::CharsetIndex::G0,
                    StandardCharset::Ascii,
                );
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, osc: &[&[u8]], _bell_terminated: bool) {
        if osc.is_empty() {
            return;
        }
        let ps = osc[0];
        let payload = osc.get(1).copied().unwrap_or(&[]);
        match ps {
            // OSC 0 / 1 / 2：窗口 / 图标 / 窗口标题。
            b"0" | b"1" | b"2" => {
                let title = String::from_utf8_lossy(payload).into_owned();
                self.term.set_title(Some(title));
            }
            // OSC 52：剪贴板。由 Term 的 Handler 直接处理（会触发 ClipboardStore 事件）。
            b"52" => {
                if let Some(sel) = payload.split(|b| b == &b';').nth(1) {
                    self.term.clipboard_store(52, sel);
                }
            }
            _ => {}
        }
    }
}

impl<'a, T: EventListener> Performer<'a, T> {
    /// CSI Ps b：重复上一个图形字符。
    fn repeat_char(&mut self, _count: usize) {
        // alacritty_terminal 0.26 未暴露 REP 处理的 Handler 入口，这里暂不实现。
        // 影响极小（极少数程序使用 REP），PoC 阶段可接受。
    }

    /// CSI … m：SGR 属性设置。
    ///
    /// 注意 vte 中 `;` 分隔的是独立参数、`:` 分隔的是子参数，两种扩展色写法都要支持：
    /// - `38;5;196` → 三个参数 [38],[5],[196]
    /// - `38:5:196` → 一个参数 [38,5,196]
    fn sgr(&mut self, params: &Params) {
        let all: Vec<&[u16]> = params.iter().collect();
        let mut i = 0;
        while i < all.len() {
            let sub = all[i];
            match sub.first().copied() {
                None | Some(0) => self.term.terminal_attribute(Attr::Reset),
                Some(1) => self.term.terminal_attribute(Attr::Bold),
                Some(2) => self.term.terminal_attribute(Attr::Dim),
                Some(3) => self.term.terminal_attribute(Attr::Italic),
                Some(4) => {
                    let style = sub.get(1).copied().unwrap_or(0);
                    let attr = match style {
                        2 => Attr::DoubleUnderline,
                        3 => Attr::Undercurl,
                        4 => Attr::DottedUnderline,
                        5 => Attr::DashedUnderline,
                        _ => Attr::Underline,
                    };
                    self.term.terminal_attribute(attr);
                }
                Some(5) => self.term.terminal_attribute(Attr::BlinkSlow),
                Some(6) => self.term.terminal_attribute(Attr::BlinkFast),
                Some(7) => self.term.terminal_attribute(Attr::Reverse),
                Some(8) => self.term.terminal_attribute(Attr::Hidden),
                Some(9) => self.term.terminal_attribute(Attr::Strike),
                Some(21) => self.term.terminal_attribute(Attr::DoubleUnderline),
                Some(22) => self.term.terminal_attribute(Attr::CancelBoldDim),
                Some(23) => self.term.terminal_attribute(Attr::CancelItalic),
                Some(24) => self.term.terminal_attribute(Attr::CancelUnderline),
                Some(25) => self.term.terminal_attribute(Attr::CancelBlink),
                Some(27) => self.term.terminal_attribute(Attr::CancelReverse),
                Some(28) => self.term.terminal_attribute(Attr::CancelHidden),
                Some(29) => self.term.terminal_attribute(Attr::CancelStrike),
                Some(30..=37) => {
                    let color = Color::Named(into_named(sub[0] as u8 - 30));
                    self.term.terminal_attribute(Attr::Foreground(color));
                }
                Some(38) => {
                    if let (Some(color), consumed) = extended_color(&all, i) {
                        self.term.terminal_attribute(Attr::Foreground(color));
                        i = consumed.saturating_sub(1);
                    }
                }
                Some(39) => {
                    let color = Color::Named(NamedColor::Foreground);
                    self.term.terminal_attribute(Attr::Foreground(color));
                }
                Some(40..=47) => {
                    let color = Color::Named(into_named(sub[0] as u8 - 40));
                    self.term.terminal_attribute(Attr::Background(color));
                }
                Some(48) => {
                    if let (Some(color), consumed) = extended_color(&all, i) {
                        self.term.terminal_attribute(Attr::Background(color));
                        i = consumed.saturating_sub(1);
                    }
                }
                Some(49) => {
                    let color = Color::Named(NamedColor::Background);
                    self.term.terminal_attribute(Attr::Background(color));
                }
                Some(90..=97) => {
                    let color = Color::Named(into_named(8 + sub[0] as u8 - 90));
                    self.term.terminal_attribute(Attr::Foreground(color));
                }
                Some(100..=107) => {
                    let color = Color::Named(into_named(8 + sub[0] as u8 - 100));
                    self.term.terminal_attribute(Attr::Background(color));
                }
                Some(_) => {}
            }
            i += 1;
        }
    }
}

/// 解析 CSI 38/48 扩展色，返回（颜色，已消费的参数总数）。
///
/// 支持两种写法：
/// - 分号参数：`38;5;n`、`38;2;r;g;b`（消费 3 / 5 个参数）
/// - 冒号子参数：`38:5:n`、`38:2:r:g:b`（消费 1 个参数）
fn extended_color(all: &[&[u16]], idx: usize) -> (Option<Color>, usize) {
    let sub = all[idx];
    // 冒号子参数形式。
    match sub.get(1).copied() {
        Some(5) => {
            return (
                sub.get(2).map(|n| Color::Indexed(*n as u8)),
                idx + 1,
            );
        }
        Some(2)
            if sub.len() >= 5 => {
                let color = Color::Spec(alacritty_terminal::vte::ansi::Rgb {
                    r: sub[2] as u8,
                    g: sub[3] as u8,
                    b: sub[4] as u8,
                });
                return (Some(color), idx + 1);
            }
        _ => {}
    }
    // 分号参数形式。
    match all.get(idx + 1).and_then(|p| p.first()).copied() {
        Some(5) => {
            let n = all.get(idx + 2).and_then(|p| p.first()).copied();
            let consumed = if n.is_some() { idx + 3 } else { idx + 1 };
            (n.map(|n| Color::Indexed(n as u8)), consumed)
        }
        Some(2) => {
            let r = all.get(idx + 2).and_then(|p| p.first()).copied();
            let g = all.get(idx + 3).and_then(|p| p.first()).copied();
            let b = all.get(idx + 4).and_then(|p| p.first()).copied();
            match (r, g, b) {
                (Some(r), Some(g), Some(b)) => {
                    let color = Color::Spec(alacritty_terminal::vte::ansi::Rgb {
                        r: r as u8,
                        g: g as u8,
                        b: b as u8,
                    });
                    (Some(color), idx + 5)
                }
                _ => (None, idx + 1),
            }
        }
        _ => (None, idx + 1),
    }
}

/// 0..=15 → NamedColor。
fn into_named(index: u8) -> NamedColor {
    match index {
        0 => NamedColor::Black,
        1 => NamedColor::Red,
        2 => NamedColor::Green,
        3 => NamedColor::Yellow,
        4 => NamedColor::Blue,
        5 => NamedColor::Magenta,
        6 => NamedColor::Cyan,
        7 => NamedColor::White,
        8 => NamedColor::BrightBlack,
        9 => NamedColor::BrightRed,
        10 => NamedColor::BrightGreen,
        11 => NamedColor::BrightYellow,
        12 => NamedColor::BrightBlue,
        13 => NamedColor::BrightMagenta,
        14 => NamedColor::BrightCyan,
        _ => NamedColor::BrightWhite,
    }
}

/// CSI Ps 参数取值，缺省为 0（由调用方决定默认语义）。
fn param(params: &Params, index: usize) -> Option<u16> {
    params.iter().nth(index).and_then(|sub| sub.first()).copied()
}

fn param_or(params: &Params, index: usize, default: u16) -> u16 {
    param(params, index).unwrap_or(default)
}

/// 移动类命令的参数计数：缺省 / 0 一律按 1。
fn count(params: &Params, index: usize) -> usize {
    match param(params, index) {
        None | Some(0) => 1,
        Some(n) => n as usize,
    }
}

/// CUP 行参数：1-based → 0-based，缺省 1。
fn row(params: &Params) -> usize {
    match param(params, 0) {
        None | Some(0) => 0,
        Some(n) => (n - 1) as usize,
    }
}

/// CUP 列参数：1-based → 0-based，缺省 1。
fn col(params: &Params) -> usize {
    match param(params, 1) {
        None | Some(0) => 0,
        Some(n) => (n - 1) as usize,
    }
}

/// CSI h/l/p 的公开模式：`CSI Ps h` 的 Ps 即模式号。
fn public_mode(mode: u16) -> alacritty_terminal::vte::ansi::Mode {
    use alacritty_terminal::vte::ansi::{Mode, NamedMode};
    match mode {
        4 => Mode::Named(NamedMode::Insert),
        20 => Mode::Named(NamedMode::LineFeedNewLine),
        _ => Mode::Unknown(mode),
    }
}

/// CSI ? Ps h/l/p 的私有模式：`CSI ? Ps h` 的 Ps 即私有模式号。
fn private_mode(mode: u16) -> alacritty_terminal::vte::ansi::PrivateMode {
    use alacritty_terminal::vte::ansi::{NamedPrivateMode, PrivateMode};
    match mode {
        1 => PrivateMode::Named(NamedPrivateMode::CursorKeys),
        3 => PrivateMode::Named(NamedPrivateMode::ColumnMode),
        6 => PrivateMode::Named(NamedPrivateMode::Origin),
        7 => PrivateMode::Named(NamedPrivateMode::LineWrap),
        12 => PrivateMode::Named(NamedPrivateMode::BlinkingCursor),
        25 => PrivateMode::Named(NamedPrivateMode::ShowCursor),
        1000 => PrivateMode::Named(NamedPrivateMode::ReportMouseClicks),
        1002 => PrivateMode::Named(NamedPrivateMode::ReportCellMouseMotion),
        1003 => PrivateMode::Named(NamedPrivateMode::ReportAllMouseMotion),
        1004 => PrivateMode::Named(NamedPrivateMode::ReportFocusInOut),
        1005 => PrivateMode::Named(NamedPrivateMode::Utf8Mouse),
        1006 => PrivateMode::Named(NamedPrivateMode::SgrMouse),
        1007 => PrivateMode::Named(NamedPrivateMode::AlternateScroll),
        1042 => PrivateMode::Named(NamedPrivateMode::UrgencyHints),
        1049 => PrivateMode::Named(NamedPrivateMode::SwapScreenAndSetRestoreCursor),
        2004 => PrivateMode::Named(NamedPrivateMode::BracketedPaste),
        2026 => PrivateMode::Named(NamedPrivateMode::SyncUpdate),
        _ => PrivateMode::Unknown(mode),
    }
}

/// 终端模型：状态 + 字节流入口 + 内容快照。
pub struct TerminalModel {
    term: Term<EventSink>,
    parser: Parser,
    events: Receiver<TerminalEvent>,
}

impl TerminalModel {
    /// 以给定尺寸创建终端。
    pub fn new(size: TerminalSize) -> Self {
        let (tx, rx) = mpsc::channel();
        let sink = EventSink::new(tx);
        let term = Term::new(Config::default(), &size, sink);
        Self {
            term,
            parser: Parser::new(),
            events: rx,
        }
    }

    /// 喂入远程 / PTY 字节流。
    pub fn process_bytes(&mut self, bytes: &[u8]) {
        let mut performer = Performer { term: &mut self.term };
        self.parser.advance(&mut performer, bytes);
    }

    /// 调整网格尺寸。
    pub fn resize(&mut self, size: TerminalSize) {
        self.term.resize(size);
    }

    /// 取出终端事件队列中的待处理事件。
    pub fn drain_events(&mut self) -> Vec<TerminalEvent> {
        self.events.try_iter().collect()
    }

    /// 当前网格尺寸。
    pub fn size(&self) -> TerminalSize {
        TerminalSize {
            columns: self.term.columns(),
            rows: self.term.screen_lines(),
        }
    }

    /// 滚动缓冲中的历史行数。
    pub fn scrollback_lines(&self) -> usize {
        self.term.grid().history_size()
    }

    /// 渲染快照：可见网格、光标、选区、颜色表。
    pub fn renderable_content(&self) -> RenderableContent<'_> {
        self.term.renderable_content()
    }

    /// 光标闪烁 / 形状等状态变更后是否有内容需要重绘。
    pub fn cursor_needs_redraw(&self) -> bool {
        // PoC 简化：始终允许重绘，由渲染层按脏区策略节流。
        true
    }

    /// 在可视区域内搜索（大小写不敏感子串），返回全部匹配。
    /// `line` 为网格原始行号（渲染器按 `point.line.0` 匹配，配合 display_offset 归一化）。
    pub fn find_matches(&self, query: &str) -> Vec<SearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let q: Vec<char> = query.to_lowercase().chars().collect();
        if q.is_empty() {
            return Vec::new();
        }
        let content = self.term.renderable_content();
        let mut lines: std::collections::BTreeMap<i32, Vec<char>> = Default::default();
        for idx in content.display_iter {
            lines.entry(idx.point.line.0).or_default().push(idx.cell.c);
        }
        let mut matches = Vec::new();
        for (line, chars) in &lines {
            let lower: Vec<char> = chars
                .iter()
                .map(|c| c.to_lowercase().next().unwrap_or(*c))
                .collect();
            if q.len() > lower.len() {
                continue;
            }
            let mut start = 0;
            while start + q.len() <= lower.len() {
                if lower[start..start + q.len()] == q[..] {
                    matches.push(SearchMatch {
                        line: *line,
                        start_col: start,
                        end_col: start + q.len(),
                    });
                    start += 1;
                } else {
                    start += 1;
                }
            }
        }
        matches
    }

    /// 滚动使匹配项可见（滚动到其所在行）。
    pub fn scroll_to_match(&mut self, m: &SearchMatch) {
        use alacritty_terminal::index::{Column, Line, Point};
        self.term.scroll_to_point(Point::new(Line(m.line), Column(m.start_col)));
    }

    /// 导出可见区域纯文本（每行去尾部空格，行间 \n）。用于"保存输出"（功能清单 2.24）。
    pub fn dump_visible_text(&self) -> String {
        let content = self.term.renderable_content();
        let mut lines: std::collections::BTreeMap<i32, String> = Default::default();
        for idx in content.display_iter {
            lines
                .entry(idx.point.line.0)
                .or_default()
                .push(idx.cell.c);
        }
        lines
            .into_values()
            .map(|s| s.trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// 语义高亮标记（功能清单 2.8）：扫描可视行内容，
    /// 含 `[ERROR]`/`ERROR` → 1，`[WARN]`/`WARN` → 2，其他 → 不标记。
    /// 返回 (行号 → 标记)。
    pub fn semantic_marks(&self) -> std::collections::HashMap<i32, u8> {
        let content = self.term.renderable_content();
        let mut lines: std::collections::BTreeMap<i32, String> = Default::default();
        for idx in content.display_iter {
            lines
                .entry(idx.point.line.0)
                .or_default()
                .push(idx.cell.c);
        }
        lines
            .into_iter()
            .filter_map(|(line, text)| {
                let upper = text.to_ascii_uppercase();
                let mark = if upper.contains("[ERROR]") || upper.contains(" ERROR") {
                    Some(1u8)
                } else if upper.contains("[WARN]") || upper.contains(" WARN") {
                    Some(2u8)
                } else {
                    None
                };
                mark.map(|m| (line, m))
            })
            .collect()
    }
}

/// 终端搜索匹配（显示坐标系）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchMatch {
    /// 网格原始行号。
    pub line: i32,
    /// 起始列（含）。
    pub start_col: usize,
    /// 结束列（不含）。
    pub end_col: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取指定行的纯文本（去掉尾部空格）。
    fn line_text(model: &TerminalModel, line: usize) -> String {
        let mut s = String::new();
        for idx in model.renderable_content().display_iter {
            if idx.point.line.0 == line as i32 {
                s.push(idx.cell.c);
            }
        }
        s.trim_end().to_string()
    }

    /// 取指定单元格的前景色。
    fn cell_fg(model: &TerminalModel, line: usize, col: usize) -> Option<Color> {
        model
            .renderable_content()
            .display_iter
            .find(|idx| idx.point.line.0 == line as i32 && idx.point.column.0 == col)
            .map(|idx| idx.cell.fg)
    }

    #[test]
    fn feed_text_and_newline() {
        let mut m = TerminalModel::new(TerminalSize::new(10, 5));
        m.process_bytes(b"Hello\r\nWorld");
        assert_eq!(line_text(&m, 0), "Hello");
        assert_eq!(line_text(&m, 1), "World");
    }

    #[test]
    fn ansi_foreground_color() {
        let mut m = TerminalModel::new(TerminalSize::new(10, 5));
        m.process_bytes(b"\x1b[31mRed\x1b[0mplain");
        assert_eq!(cell_fg(&m, 0, 0), Some(Color::Named(NamedColor::Red)));
        // SGR 0 之后恢复默认前景。
        assert_eq!(
            cell_fg(&m, 0, 3),
            Some(Color::Named(NamedColor::Foreground))
        );
    }

    #[test]
    fn ansi_256_color() {
        let mut m = TerminalModel::new(TerminalSize::new(10, 5));
        m.process_bytes(b"\x1b[38;5;196mX");
        assert_eq!(cell_fg(&m, 0, 0), Some(Color::Indexed(196)));
    }

    #[test]
    fn cursor_backward_overwrite() {
        let mut m = TerminalModel::new(TerminalSize::new(10, 5));
        // 写 AB，光标左移 2，再写 XY —— 应覆盖成 XY。
        m.process_bytes(b"AB\x1b[2DXY");
        assert_eq!(line_text(&m, 0), "XY");
    }

    #[test]
    fn scrollback_accumulates() {
        let mut m = TerminalModel::new(TerminalSize::new(6, 2));
        for i in 0..5 {
            m.process_bytes(format!("line{i}\r\n").as_bytes());
        }
        // 5 行内容、2 行屏幕：最后一次 LF 也触发滚动 → 4 行进入滚动缓冲。
        assert_eq!(m.scrollback_lines(), 4);
        // 滚动后 line4 位于第 0 行。
        assert_eq!(line_text(&m, 0), "line4");
    }

    #[test]
    fn erase_in_display() {
        let mut m = TerminalModel::new(TerminalSize::new(10, 5));
        m.process_bytes(b"ABCDEF\x1b[2J");
        // ED 2：清空整个屏幕，首行应为空。
        assert_eq!(line_text(&m, 0), "");
        assert_eq!(line_text(&m, 3), "");
    }

    #[test]
    fn scroll_region_and_alternate_screen() {
        let mut m = TerminalModel::new(TerminalSize::new(10, 5));
        // 进入 alternate screen 并写入内容。
        m.process_bytes(b"\x1b[?1049hhello-altscreen");
        // 退出 alternate screen 恢复主屏。
        m.process_bytes(b"\x1b[?1049l");
        assert_eq!(line_text(&m, 0), "");
    }

    #[test]
    fn find_matches_locates_substring_case_insensitive() {
        let mut m = TerminalModel::new(TerminalSize::new(80, 24));
        m.process_bytes(b"hello world\r\nfoo bar hello\r\n");
        // 大小写不敏感。
        let ms = m.find_matches("HELLO");
        assert_eq!(ms.len(), 2, "两处 hello：{ms:?}");
        assert!(ms.iter().any(|x| x.start_col == 0 && x.end_col == 5));
        assert!(ms.iter().any(|x| x.start_col == 8 && x.end_col == 13));
        // 空查询 → 无匹配。
        assert!(m.find_matches("").is_empty());
        // 无命中。
        assert!(m.find_matches("zzz").is_empty());
    }

    #[test]
    fn semantic_marks_flags_error_and_warn_lines() {
        let mut m = TerminalModel::new(TerminalSize::new(80, 24));
        m.process_bytes(b"[OK] fine\r\n[ERROR] boom\r\n[WARN] slow\r\n");
        let marks = m.semantic_marks();
        assert_eq!(
            marks.values().filter(|&&v| v == 1).count(),
            1,
            "一条 ERROR 行：{marks:?}"
        );
        assert_eq!(
            marks.values().filter(|&&v| v == 2).count(),
            1,
            "一条 WARN 行：{marks:?}"
        );
    }

    #[test]
    fn dump_visible_text_joins_lines() {
        let mut m = TerminalModel::new(TerminalSize::new(40, 10));
        m.process_bytes(b"alpha\r\nbeta\r\n");
        let text = m.dump_visible_text();
        assert!(text.contains("alpha"), "文本：{text:?}");
        assert!(text.contains("beta"), "文本：{text:?}");
    }
}
