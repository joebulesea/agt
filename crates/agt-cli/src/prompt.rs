//! 互動式選單（類似 Claude Code 的初次設定）：↑↓ 移動、空白鍵勾選、Enter 確認、Esc 取消。
//! 直接畫在目前的終端機行上（不切換全螢幕），每次按鍵後重畫選單區塊。

use crossterm::cursor::{Hide, MoveToColumn, MoveUp, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor, Stylize,
};
use crossterm::terminal::{self, Clear, ClearType};
use crossterm::{execute, queue};
use std::io::{IsTerminal, Write, stdout};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 使用者按了 Esc 或 Ctrl+C。
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("已取消設定")
    }
}
impl std::error::Error for Cancelled {}

pub struct Item<'a> {
    pub label: &'a str,
    pub desc: &'a str,
}

/// 依顯示寬度截斷（全形字佔 2 欄），避免折行打亂重畫的行數。
fn fit(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// 原始模式期間的保護：離開（含 panic、提早 return）時一定恢復終端機。
struct Raw;
impl Raw {
    fn on() -> std::io::Result<Raw> {
        terminal::enable_raw_mode()?;
        execute!(stdout(), Hide)?;
        Ok(Raw)
    }
}
impl Drop for Raw {
    fn drop(&mut self) {
        let _ = execute!(stdout(), Show);
        let _ = terminal::disable_raw_mode();
    }
}

/// 一行的內容與樣式。
enum Line {
    Title(String),
    Hint(String),
    Option {
        cursor: bool,
        mark: Option<bool>,
        label: String,
        desc: String,
    },
}

/// 重畫：游標先回到上一次畫的第一行，清掉以下內容再畫。
fn draw(lines: &[Line], prev: &mut u16) -> std::io::Result<()> {
    let width = terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
        .max(20)
        - 1;
    let mut out = stdout();
    queue!(out, MoveToColumn(0))?;
    if *prev > 0 {
        queue!(out, MoveUp(*prev))?;
    }
    queue!(out, Clear(ClearType::FromCursorDown))?;
    for l in lines {
        match l {
            Line::Title(t) => queue!(out, Print(fit(t, width).bold()))?,
            Line::Hint(t) => queue!(out, Print(fit(t, width).dark_grey()))?,
            Line::Option {
                cursor,
                mark,
                label,
                desc,
            } => {
                let pointer = if *cursor { "❯ " } else { "  " };
                let check = match mark {
                    Some(true) => "◉ ",
                    Some(false) => "◯ ",
                    None => "",
                };
                let head = format!("{pointer}{check}{label}");
                let color = if *cursor { Color::Cyan } else { Color::Reset };
                queue!(
                    out,
                    SetForegroundColor(color),
                    Print(fit(&head, width)),
                    ResetColor
                )?;
                let used = head.width();
                if !desc.is_empty() && used + 4 < width {
                    queue!(
                        out,
                        Print("  "),
                        Print(fit(desc, width - used - 2).dark_grey())
                    )?;
                }
            }
        }
        queue!(out, Print("\r\n"))?;
    }
    out.flush()?;
    *prev = lines.len() as u16;
    Ok(())
}

fn read_key() -> std::io::Result<KeyEvent> {
    loop {
        if let Event::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            return Ok(k);
        }
    }
}

fn is_cancel(k: &KeyEvent) -> bool {
    k.code == KeyCode::Esc
        || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL))
}

/// 單選：回傳選到的索引。
pub fn select(title: &str, items: &[Item], default: usize) -> anyhow::Result<usize> {
    let mut cur = default.min(items.len().saturating_sub(1));
    let mut prev = 0;
    let _raw = Raw::on()?;
    loop {
        let mut lines = vec![Line::Title(title.to_string())];
        for (i, it) in items.iter().enumerate() {
            lines.push(Line::Option {
                cursor: i == cur,
                mark: None,
                label: format!("{}. {}", i + 1, it.label),
                desc: it.desc.to_string(),
            });
        }
        lines.push(Line::Hint("↑↓ 選擇 · Enter 確認 · Esc 取消".into()));
        draw(&lines, &mut prev)?;
        let k = read_key()?;
        match k.code {
            _ if is_cancel(&k) => return Err(Cancelled.into()),
            KeyCode::Up | KeyCode::Char('k') => cur = (cur + items.len() - 1) % items.len(),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => cur = (cur + 1) % items.len(),
            KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < items.len() => {
                cur = c as usize - '1' as usize;
            }
            KeyCode::Enter => break,
            _ => {}
        }
    }
    // 收起選單，只留下結果
    let lines = vec![Line::Title(format!("{title} {}", items[cur].label.green()))];
    draw(&lines, &mut prev)?;
    Ok(cur)
}

/// 多選：回傳每一項是否勾選。
pub fn multi_select(title: &str, items: &[Item], checked: &[bool]) -> anyhow::Result<Vec<bool>> {
    let mut on = checked.to_vec();
    let mut cur = 0;
    let mut prev = 0;
    let _raw = Raw::on()?;
    loop {
        let mut lines = vec![Line::Title(title.to_string())];
        for (i, it) in items.iter().enumerate() {
            lines.push(Line::Option {
                cursor: i == cur,
                mark: Some(on[i]),
                label: it.label.to_string(),
                desc: it.desc.to_string(),
            });
        }
        lines.push(Line::Hint(
            "↑↓ 移動 · 空白鍵 勾選 · Enter 確認 · Esc 取消".into(),
        ));
        draw(&lines, &mut prev)?;
        let k = read_key()?;
        match k.code {
            _ if is_cancel(&k) => return Err(Cancelled.into()),
            KeyCode::Up | KeyCode::Char('k') => cur = (cur + items.len() - 1) % items.len(),
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => cur = (cur + 1) % items.len(),
            KeyCode::Char(' ') => on[cur] = !on[cur],
            KeyCode::Enter => break,
            _ => {}
        }
    }
    let picked: Vec<&str> = items
        .iter()
        .zip(&on)
        .filter(|(_, o)| **o)
        .map(|(it, _)| it.label)
        .collect();
    let summary = if picked.is_empty() {
        "（無）".to_string()
    } else {
        picked.join("、")
    };
    let lines = vec![Line::Title(format!("{title} {}", summary.green()))];
    draw(&lines, &mut prev)?;
    Ok(on)
}

/// 歡迎畫面：圓角框，類似 Claude Code 的開場。
pub fn banner(title: &str, subtitle: &str) {
    let inner = title.width().max(subtitle.width()) + 4;
    let line = "─".repeat(inner);
    let pad = |s: &str| format!("{s}{}", " ".repeat(inner - 2 - s.width()));
    let mut out = stdout();
    let _ = queue!(
        out,
        SetForegroundColor(Color::Rgb {
            r: 215,
            g: 119,
            b: 87
        }),
        Print(format!("╭{line}╮\r\n")),
        Print("│ "),
        SetAttribute(Attribute::Bold),
        Print(pad(title)),
        SetAttribute(Attribute::Reset),
        SetForegroundColor(Color::Rgb {
            r: 215,
            g: 119,
            b: 87
        }),
        Print(" │\r\n│ "),
        ResetColor,
        Print(pad(subtitle).dark_grey()),
        SetForegroundColor(Color::Rgb {
            r: 215,
            g: 119,
            b: 87
        }),
        Print(format!(" │\r\n╰{line}╯\r\n")),
        ResetColor,
    );
    let _ = out.flush();
}

/// 顯示「按任意鍵關閉」並等待按鍵（從檔案總管雙擊開啟時，避免視窗直接關閉看不到訊息）。
pub fn pause() {
    if !std::io::stdin().is_terminal() {
        return;
    }
    print!("\n{}", "按任意鍵關閉…".dark_grey());
    let _ = stdout().flush();
    if let Ok(_raw) = Raw::on() {
        let _ = read_key();
    }
    println!();
}

/// 狀態列：✓ / ! / ✗ 加上顏色。
pub fn status(kind: char, text: &str) {
    let mark = match kind {
        '✓' => "✓".green(),
        '!' => "!".yellow(),
        _ => "✗".red(),
    };
    println!("  {mark} {text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_truncates_by_display_width() {
        assert_eq!(fit("abc", 10), "abc");
        assert_eq!(fit("中文字串", 5), "中文…");
        assert!(fit("abcdefghij", 5).width() <= 5);
    }
}
