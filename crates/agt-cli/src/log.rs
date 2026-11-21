//! `agt log <id>`：把分頁的終端機紀錄檔整理成純文字或 Markdown 匯出。
//!
//! 紀錄檔是 Agent 輸出的原始位元組（含顏色、游標移動等控制序列）。這裡只做「讀得懂」的整理：
//! 去掉控制序列、處理 `\r` 與退格造成的覆寫（例如進度列），不嘗試重現完整的畫面。

use agt_proto::ClientMsg;
use agt_proto::client::expect_session;
use anyhow::{Context, bail};

/// `agt log` 的參數。
pub struct Options {
    pub id: String,
    pub tail: Option<usize>,
    /// 原始位元組（含控制序列），只能寫到檔案
    pub raw: bool,
    pub output: Option<String>,
    /// "text" | "md"
    pub format: String,
    /// 覆寫已存在的輸出檔
    pub force: bool,
}

pub async fn run(o: Options) -> anyhow::Result<()> {
    if o.raw && o.output.is_none() {
        // 原始輸出可能含有改寫剪貼簿、視窗標題的控制序列，不直接送到終端機
        bail!("--raw 需要搭配 -o <檔案>（原始輸出含控制序列，不直接顯示在終端機）");
    }
    if o.raw && (o.format == "md" || o.tail.is_some()) {
        bail!("--raw 不能搭配 --format md 或 --tail");
    }
    let c = crate::ensure_daemon("cli").await?.client;
    let s = expect_session(
        c.request(ClientMsg::GetSession { id: o.id.clone() })
            .await?,
    )?;
    let files = agt_daemon::session_log_files(&s.id);
    if files.is_empty() {
        bail!("找不到分頁 {} 的紀錄檔（設定檔的 log_output 是否為 false？）", s.short_id);
    }
    let mut bytes = Vec::new();
    for f in &files {
        bytes.extend(std::fs::read(f).with_context(|| format!("無法讀取 {}", f.display()))?);
    }
    let payload: Vec<u8> = if o.raw {
        bytes
    } else {
        let mut text = strip_ansi(&bytes);
        if let Some(n) = o.tail {
            text = tail_lines(&text, n);
        }
        if o.format == "md" {
            to_markdown(&s.task, &s.agent, &s.short_id, s.branch.as_deref(), &text).into_bytes()
        } else {
            text.into_bytes()
        }
    };
    match &o.output {
        Some(path) => {
            let mut f = std::fs::OpenOptions::new();
            f.write(true);
            if o.force {
                f.create(true).truncate(true);
            } else {
                f.create_new(true);
            }
            let mut f = f.open(path).with_context(|| {
                if !o.force && std::path::Path::new(path).exists() {
                    format!("{path} 已存在（要覆寫請加 --force）")
                } else {
                    format!("無法寫入 {path}")
                }
            })?;
            std::io::Write::write_all(&mut f, &payload)?;
            eprintln!("已寫入 {path}（{} 位元組）", payload.len());
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(&payload)?;
        }
    }
    Ok(())
}

/// 去掉控制序列並還原成純文字。
pub fn strip_ansi(input: &[u8]) -> String {
    let text = String::from_utf8_lossy(input);
    let mut lines: Vec<Vec<char>> = vec![Vec::new()];
    let mut col = 0usize;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\x1b' => match it.next() {
                // CSI：參數與中間字元到結尾字元（0x40–0x7e）為止
                Some('[') => {
                    let mut params = String::new();
                    for n in it.by_ref() {
                        if ('\x40'..='\x7e').contains(&n) {
                            // 清除到行尾／整行；其他游標與顏色控制一律忽略
                            if n == 'K' {
                                let line = lines.last_mut().unwrap();
                                match params.as_str() {
                                    "" | "0" => line.truncate(col),
                                    "2" => {
                                        line.clear();
                                        col = 0;
                                    }
                                    _ => {}
                                }
                            }
                            break;
                        }
                        params.push(n);
                    }
                }
                // OSC：到 BEL 或 ST（ESC \）為止
                Some(']') => skip_string(&mut it, true),
                // DCS、SOS、PM、APC：到 ST 為止
                Some('P' | 'X' | '^' | '_') => skip_string(&mut it, false),
                // 字元集指定（ESC ( B 等）：再吃一個字元
                Some('(' | ')' | '*' | '+' | '#' | '%') => {
                    it.next();
                }
                // 其他單一字元的 ESC 序列（ESC 7、ESC =、ESC M…）
                Some(_) | None => {}
            },
            '\n' => {
                lines.push(Vec::new());
                col = 0;
            }
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            '\t' => put(lines.last_mut().unwrap(), &mut col, '\t'),
            c if c.is_control() => {}
            c => put(lines.last_mut().unwrap(), &mut col, c),
        }
    }
    let mut out: Vec<String> = lines
        .into_iter()
        .map(|l| l.into_iter().collect::<String>().trim_end().to_string())
        .collect();
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        return String::new();
    }
    out.join("\n") + "\n"
}

fn put(line: &mut Vec<char>, col: &mut usize, c: char) {
    if *col < line.len() {
        line[*col] = c;
    } else {
        line.push(c);
    }
    *col += 1;
}

/// 跳過 OSC／DCS 這類字串序列。`bel`：OSC 也可以用 BEL 結尾。
fn skip_string(it: &mut std::iter::Peekable<std::str::Chars<'_>>, bel: bool) {
    while let Some(c) = it.next() {
        if bel && c == '\x07' {
            return;
        }
        if c == '\x1b' && it.peek() == Some(&'\\') {
            it.next();
            return;
        }
    }
}

/// 只保留最後 `n` 行。
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut out = lines[start..].join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// 包住內容的 Markdown 圍欄：比內容裡最長的連續反引號再多一個（至少 3 個）。
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

/// Markdown 版：標題、分頁資訊與圍起來的內容。
pub fn to_markdown(
    task: &str,
    agent: &str,
    short_id: &str,
    branch: Option<&str>,
    text: &str,
) -> String {
    let task = agt_proto::text::printable_line(task);
    let fence = fence_for(text);
    let mut out = format!("# agt 分頁紀錄：{task}\n\n- Agent：{agent}\n- 分頁：{short_id}\n");
    if let Some(b) = branch {
        out.push_str(&format!("- Branch：{b}\n"));
    }
    out.push_str(&format!("\n{fence}text\n{text}{fence}\n"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_colors_and_cursor_controls() {
        let raw = b"\x1b[1;32mgreen\x1b[0m plain \x1b[2J\x1b[H\x1b[?25lend\n";
        assert_eq!(strip_ansi(raw), "green plain end\n");
    }

    #[test]
    fn strips_osc_and_other_strings() {
        // 標題（BEL 結尾）、超連結與剪貼簿（ST 結尾）、DCS
        let raw = b"a\x1b]0;title\x07b\x1b]52;c;ZXZpbA==\x1b\\c\x1bPq\x1b\\d\n";
        assert_eq!(strip_ansi(raw), "abcd\n");
        // 不完整的序列不會讓整段消失
        assert_eq!(strip_ansi(b"ok\x1b"), "ok\n");
    }

    #[test]
    fn carriage_return_overwrites_the_line() {
        // 進度列：後面的文字蓋掉前面的（沒有清除時，較短的新文字只蓋住開頭）
        assert_eq!(strip_ansi(b"loading 10%\rloading 90%\n"), "loading 90%\n");
        assert_eq!(strip_ansi(b"abcdef\rXY\n"), "XYcdef\n");
        // CRLF 只是換行
        assert_eq!(strip_ansi(b"abc\r\nnext\r\n"), "abc\nnext\n");
    }

    #[test]
    fn erase_to_end_of_line_clears_leftovers() {
        assert_eq!(strip_ansi(b"long text\rhi\x1b[K\n"), "hi\n");
        assert_eq!(strip_ansi(b"wipe me\x1b[2K\rnew\n"), "new\n");
    }

    #[test]
    fn backspace_moves_back_and_overwrites() {
        assert_eq!(strip_ansi(b"abc\x08\x08X\n"), "aXc\n");
    }

    #[test]
    fn other_control_characters_are_dropped_but_tabs_stay() {
        assert_eq!(strip_ansi(b"a\x00\x07\x7fb\tc\n"), "ab\tc\n");
    }

    #[test]
    fn invalid_utf8_and_unicode() {
        assert_eq!(strip_ansi("中文 ✓\n".as_bytes()), "中文 ✓\n");
        let s = strip_ansi(b"ok\xff\xfe end\n");
        assert!(s.starts_with("ok") && s.ends_with("end\n"), "{s:?}");
    }

    #[test]
    fn trailing_blank_lines_and_spaces_are_trimmed() {
        assert_eq!(strip_ansi(b"a   \n\n\n"), "a\n");
        assert_eq!(strip_ansi(b""), "");
        assert_eq!(strip_ansi(b"\x1b[0m\n\n"), "");
    }

    #[test]
    fn tail_keeps_last_lines() {
        let t = "1\n2\n3\n4\n";
        assert_eq!(tail_lines(t, 2), "3\n4\n");
        assert_eq!(tail_lines(t, 10), t);
        assert_eq!(tail_lines(t, 0), "");
        assert_eq!(tail_lines("", 3), "");
    }

    #[test]
    fn markdown_fence_is_longer_than_any_backtick_run() {
        let md = to_markdown("修 bug", "claude", "ab12", Some("agt/claude/x"), "```rust\nfn f(){}\n```\n");
        assert!(md.starts_with("# agt 分頁紀錄：修 bug\n"), "{md}");
        assert!(md.contains("- Agent：claude") && md.contains("- 分頁：ab12"));
        assert!(md.contains("- Branch：agt/claude/x"));
        // 內容有 ```，所以外層至少 ````
        assert!(md.contains("````text\n```rust"), "{md}");
        assert!(md.trim_end().ends_with("````"), "{md}");
        let plain = to_markdown("t", "a", "i", None, "x\n");
        assert!(plain.contains("```text\nx\n```\n"), "{plain}");
        assert!(!plain.contains("Branch"));
    }

    #[test]
    fn markdown_title_cannot_inject_lines() {
        let md = to_markdown("a\n## 假標題\x1b[31m", "c", "i", None, "x\n");
        // 標題只佔一行：不會因為換行而多出一個 `## ` 標題行
        assert!(!md.lines().any(|l| l.starts_with("## ")), "{md}");
        assert!(!md.contains('\x1b'));
    }
}
