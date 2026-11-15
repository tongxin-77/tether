//! 会话输出历史（hub 协议能力 history）：vt100 的 scrollback 为 0，屏幕之外的内容需自行保存。
//! 在 vt100 的 on_scroll 回调中把被推出主屏顶部的行格式化存入环形缓冲（备用屏的行丢弃），
//! 原始输出中出现 `\x1b[3J` 时清空；hub 接管时按协议拼成可直接写入 xterm 的字节流。

use std::{collections::VecDeque, fmt};

use vt100::capture::{BasicFormattedCaptureState, RowContents};

/// 环形缓冲上限：行数与字节数
const MAX_LINES: usize = 5000;
const MAX_BYTES: usize = 2 << 20;
/// attached 中的 history 上限（解码后的原始字节，从最旧端截断）
const SEND_LINES: usize = 3000;
const SEND_BYTES: usize = 1 << 20;
/// 清空历史的序列（ED 3）
const CLEAR: &[u8] = b"\x1b[3J";

/// 一行历史：行首 SGR 复位 + 本行带 SGR 的内容；wrapped 为该行自动折行到下一行
struct Line {
    text: String,
    wrapped: bool,
}

#[derive(Default)]
pub struct History {
    lines: VecDeque<Line>,
    bytes: usize,
    /// 上一块输出末尾的不超过 3 个字节，检测跨块的 `\x1b[3J`
    tail: Vec<u8>,
}

/// 只记录第一次写出的内容是否以换行开头，随即中止格式化
struct FirstWrite(Option<bool>);

impl fmt::Write for FirstWrite {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0 = Some(s.starts_with('\n'));
        Err(fmt::Error)
    }
}

impl History {
    /// 被推出屏幕顶部的行（vt100 on_scroll）：主屏的存入，备用屏的丢弃
    pub fn push(&mut self, row: RowContents<'_>, alternate: bool) {
        if alternate {
            return;
        }
        let mut text = String::from("\x1b[m");
        let mut state = BasicFormattedCaptureState::new();
        let _ = row.write_formatted_basic(&mut text, &mut state);
        // 折行标志不公开：同一 state 再写一次时，未折行的行会先写出换行
        let mut probe = FirstWrite(None);
        let _ = row.write_formatted_basic(&mut probe, &mut state);
        let wrapped = probe.0 != Some(true);
        self.bytes += text.len();
        self.lines.push_back(Line { text, wrapped });
        while self.lines.len() > MAX_LINES || self.bytes > MAX_BYTES {
            let Some(old) = self.lines.pop_front() else { break };
            self.bytes -= old.text.len();
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
        self.bytes = 0;
    }

    /// 本块输出中每个 `\x1b[3J` 结束处（含与上一块拼接成的）在 data 中的位置
    fn clear_points(&mut self, data: &[u8]) -> Vec<usize> {
        let mut buf = std::mem::take(&mut self.tail);
        let carried = buf.len();
        buf.extend_from_slice(data);
        let points = buf.windows(CLEAR.len()).enumerate().filter(|(_, w)| *w == CLEAR).map(|(i, _)| i + CLEAR.len() - carried).collect();
        self.tail = buf[buf.len().saturating_sub(CLEAR.len() - 1)..].to_vec();
        points
    }

    /// 按协议拼装：历史各行（未折行的行后接 `\r\n`）→ rows 个 `\r\n` 把历史推入 scrollback → 当前在备用屏时追加 `\x1b[?1049h`。
    /// 发送最近不超过 3000 行、总字节不超过 1MB；没有历史时不推空行。screen 为该会话当前屏幕（取行数与是否备用屏）
    pub fn render(&self, screen: &vt100::Screen) -> Vec<u8> {
        let rows = screen.size().0.get();
        let alternate = screen.alternate_screen();
        let budget = SEND_BYTES - usize::from(rows) * 2 - 16;
        let (mut count, mut size) = (0, 0);
        for line in self.lines.iter().rev().take(SEND_LINES) {
            if size + line.text.len() + 2 > budget {
                break;
            }
            size += line.text.len() + 2;
            count += 1;
        }
        let mut out = Vec::with_capacity(size + usize::from(rows) * 2 + 16);
        let lines: Vec<&Line> = self.lines.iter().skip(self.lines.len() - count).collect();
        for (i, line) in lines.iter().enumerate() {
            out.extend_from_slice(line.text.as_bytes());
            if !line.wrapped && i + 1 < lines.len() {
                out.extend_from_slice(b"\r\n");
            }
        }
        if count > 0 {
            out.extend_from_slice(b"\x1b[m");
            out.extend_from_slice(&b"\r\n".repeat(usize::from(rows)));
        }
        if alternate {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out
    }
}

impl vt100::Callbacks for History {
    fn on_scroll(&mut self, row: RowContents<'_>, alternate: bool) {
        self.push(row, alternate);
    }
}

/// 喂一块输出给 parser：在每个 `\x1b[3J` 处切开，处理完该序列后清空历史（之前推出的行清掉，之后的保留）
pub fn process<CB: vt100::Callbacks + AsMut<History>>(parser: &mut vt100::Parser<CB>, data: &[u8]) {
    let mut start = 0;
    for end in parser.callbacks_mut().as_mut().clear_points(data) {
        parser.process(&data[start..end]);
        parser.callbacks_mut().as_mut().clear();
        start = end;
    }
    parser.process(&data[start..]);
}

impl AsMut<History> for History {
    fn as_mut(&mut self) -> &mut History {
        self
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU16;

    use super::*;

    fn parser(rows: u16, cols: u16) -> vt100::Parser<History> {
        vt100::Parser::new_with_callbacks(NonZeroU16::new(rows).unwrap(), NonZeroU16::new(cols).unwrap(), 0, History::default())
    }

    fn render(p: &vt100::Parser<History>) -> Vec<u8> {
        p.callbacks().render(p.screen())
    }

    fn lines(p: &mut vt100::Parser<History>) -> Vec<String> {
        p.callbacks_mut().lines.iter().map(|l| vt100::capture::basic_formatted_to_plain(&l.text).collect()).collect()
    }

    /// 把 history + snapshot 写进带大 scrollback 的新解析器（模拟浏览器 xterm）
    fn replay(p: &mut vt100::Parser<History>) -> vt100::Parser {
        let (rows, cols) = p.screen().size();
        let mut x = vt100::Parser::new(rows, cols, 100_000);
        x.process(&render(p));
        x.process(p.screen().state_formatted().as_bytes());
        x
    }

    /// x 的 scrollback 全部内容（纯文本，按行）
    fn scrollback(x: &mut vt100::Parser) -> Vec<String> {
        x.screen_mut().set_scrollback(usize::MAX);
        let n = x.screen().scrollback();
        let (rows, cols) = x.screen().size();
        let mut out = Vec::new();
        // 每次滚动一屏，取可见的 scrollback 行
        let mut offset = n;
        while offset > 0 {
            x.screen_mut().set_scrollback(offset);
            let take = offset.min(usize::from(rows.get()));
            out.extend(x.screen().rows(0, cols.get()).take(take));
            offset -= take;
        }
        x.screen_mut().set_scrollback(0);
        out
    }

    #[test]
    fn hundred_lines_on_ten_row_screen() {
        let mut p = parser(10, 40);
        for i in 0..100 {
            process(&mut p, format!("\x1b[3{}mline {i}\x1b[m\r\n", i % 8).as_bytes());
        }
        // 最后一行之后光标在空行上：屏幕保留 line 91–99 与空行，推出 0–90
        let got = lines(&mut p);
        assert_eq!(got.len(), 91);
        assert_eq!((got[0].as_str(), got[90].as_str()), ("line 0", "line 90"));
        // 颜色保留，行首复位
        let first = &p.callbacks_mut().lines[1].text;
        assert!(first.starts_with("\x1b[m") && first.contains("\x1b[31m"), "{first:?}");

        let mut x = replay(&mut p);
        assert_eq!(x.screen().state_formatted(), p.screen().state_formatted());
        let sb = scrollback(&mut x);
        // 历史行之后是推入时的 10 个空行（rows 个 \r\n 推上去的屏幕）
        assert_eq!(sb[..91], got[..]);
        assert!(sb[91..].iter().all(|l| l.is_empty()));
        // 颜色在 scrollback 中保留
        x.screen_mut().set_scrollback(sb.len());
        assert_eq!(x.screen().cell(1, 0).unwrap().fgcolor(), vt100::Color::Idx(1));
    }

    #[test]
    fn wrapped_rows_not_split() {
        let mut p = parser(3, 10);
        // 25 个字符折成 3 行，后面再输出 4 行：前 4 个物理行进入历史
        process(&mut p, b"abcdefghijklmnopqrstuvwxy\r\n1\r\n2\r\n3\r\n4");
        let h = p.callbacks_mut();
        assert_eq!(h.lines.len(), 4);
        assert!(h.lines[0].wrapped && h.lines[1].wrapped && !h.lines[2].wrapped);
        let out = String::from_utf8(render(&p)).unwrap();
        assert!(out.starts_with("\x1b[mabcdefghij\x1b[mklmnopqrst\x1b[muvwxy\r\n\x1b[m1\x1b[m\r\n"), "{out:?}");
        let mut x = replay(&mut p);
        assert_eq!(x.screen().state_formatted(), p.screen().state_formatted());
        assert_eq!(scrollback(&mut x)[..4], ["abcdefghij", "klmnopqrst", "uvwxy", "1"]);
    }

    #[test]
    fn clear_scrollback_sequence() {
        let mut p = parser(3, 20);
        process(&mut p, b"a\r\nb\r\nc\r\nd\r\n");
        assert_eq!(lines(&mut p), ["a", "b"]);
        // 同一块中：序列之前推出的行清掉，之后推出的保留
        process(&mut p, b"e\r\n\x1b[3Jf\r\ng\r\n");
        assert_eq!(lines(&mut p), ["d", "e"]);
        // 跨块：\x1b[3 | J
        process(&mut p, b"h\r\n\x1b[3");
        assert_eq!(lines(&mut p), ["d", "e", "f"]);
        process(&mut p, b"J");
        assert!(lines(&mut p).is_empty());
        // 跨三块：\x1b | [ | 3J
        process(&mut p, b"i\r\nj\r\n\x1b");
        process(&mut p, b"[");
        process(&mut p, b"3Jk\r\n");
        assert_eq!(lines(&mut p), ["i"]);
        // 不相干的 CSI 不清空
        process(&mut p, b"\x1b[2J\x1b[3m");
        assert_eq!(lines(&mut p), ["i"]);
    }

    #[test]
    fn alternate_screen_rows_dropped() {
        let mut p = parser(3, 20);
        process(&mut p, b"a\r\nb\r\nc\r\n");
        assert_eq!(lines(&mut p), ["a"]);
        process(&mut p, b"\x1b[?1049hx\r\ny\r\nz\r\nw\r\n");
        assert_eq!(lines(&mut p), ["a"]);
        let out = render(&p);
        assert!(out.ends_with(b"\r\n\x1b[?1049h"));
        let x = replay(&mut p);
        assert!(x.screen().alternate_screen());
        assert_eq!(x.screen().contents(), p.screen().contents());
        process(&mut p, b"\x1b[?1049l");
        assert!(!render(&p).ends_with(b"\x1b[?1049h"));
    }

    #[test]
    fn capped_oldest_dropped() {
        let mut p = parser(2, 20);
        for i in 0..6000 {
            process(&mut p, format!("{i}\r\n").as_bytes());
        }
        let got = lines(&mut p);
        assert_eq!(got.len(), MAX_LINES);
        assert_eq!(got.last().unwrap(), "5998");
        assert_eq!(got[0], (5999 - MAX_LINES).to_string());
        // 发送最近 3000 行
        let out = String::from_utf8(render(&p)).unwrap();
        assert_eq!(out.matches("\x1b[m").count(), SEND_LINES + 1);
        assert!(out.starts_with(&format!("\x1b[m{}\r\n", 5999 - SEND_LINES)));
        // 字节上限：每行 2000 字节以上，2MB 缓冲约 1000 行，发送不超过 1MB
        let mut p = parser(2, 1000);
        let row = "\x1b[31mx\x1b[32my".repeat(150);
        for _ in 0..3000 {
            process(&mut p, format!("{row}\r\n").as_bytes());
        }
        let h = p.callbacks_mut();
        assert!(h.bytes <= MAX_BYTES && h.lines.len() < 3000);
        assert!(render(&p).len() <= SEND_BYTES);
    }

    #[test]
    fn empty_history() {
        let mut p = parser(5, 20);
        process(&mut p, b"hi");
        assert!(render(&p).is_empty());
    }
}
