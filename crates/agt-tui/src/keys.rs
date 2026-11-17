//! 把 crossterm 的按鍵與滑鼠事件編碼成送給 PTY 的 bytes。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

/// 前綴鍵，例如 `ctrl-a`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    pub ch: char,
}

impl Prefix {
    pub fn parse(s: &str) -> Prefix {
        let lower = s.to_ascii_lowercase();
        let key = lower
            .strip_prefix("ctrl-")
            .or_else(|| lower.strip_prefix("ctrl+"))
            .or_else(|| lower.strip_prefix("c-"))
            .unwrap_or("a");
        Prefix {
            ch: key.chars().next().unwrap_or('a'),
        }
    }

    pub fn matches(&self, k: &KeyEvent) -> bool {
        k.modifiers.contains(KeyModifiers::CONTROL)
            && match k.code {
                KeyCode::Char(c) => c.eq_ignore_ascii_case(&self.ch),
                _ => false,
            }
    }

    /// 真正的控制字元（再按一次前綴鍵時送出）。
    pub fn byte(&self) -> u8 {
        ctrl_byte(self.ch).unwrap_or(1)
    }

    pub fn label(&self) -> String {
        format!("Ctrl+{}", self.ch.to_ascii_uppercase())
    }
}

fn ctrl_byte(c: char) -> Option<u8> {
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '7' | '-' => 31,
        '?' | '8' => 127,
        _ => return None,
    })
}

fn modifier_param(m: KeyModifiers) -> u8 {
    1 + (m.contains(KeyModifiers::SHIFT) as u8)
        + 2 * (m.contains(KeyModifiers::ALT) as u8)
        + 4 * (m.contains(KeyModifiers::CONTROL) as u8)
}

fn csi_letter(letter: char, m: KeyModifiers, app_cursor: bool) -> Vec<u8> {
    let p = modifier_param(m);
    if p > 1 {
        format!("\x1b[1;{p}{letter}").into_bytes()
    } else if app_cursor {
        format!("\x1bO{letter}").into_bytes()
    } else {
        format!("\x1b[{letter}").into_bytes()
    }
}

fn csi_tilde(n: u8, m: KeyModifiers) -> Vec<u8> {
    let p = modifier_param(m);
    if p > 1 {
        format!("\x1b[{n};{p}~").into_bytes()
    } else {
        format!("\x1b[{n}~").into_bytes()
    }
}

/// 把按鍵編碼成 xterm 相容的序列。
pub fn encode_key(k: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let m = k.modifiers;
    let alt = m.contains(KeyModifiers::ALT);
    let mut out = match k.code {
        KeyCode::Char(c) => {
            if m.contains(KeyModifiers::CONTROL) {
                vec![ctrl_byte(c)?]
            } else {
                let mut b = [0u8; 4];
                c.encode_utf8(&mut b).as_bytes().to_vec()
            }
        }
        // Ctrl+Enter / Shift+Enter：換行而不送出。送 LF（等同 Ctrl+J），
        // Claude Code 等 Agent 的輸入框會把它當成換行；一般 Enter 仍是 CR（送出）。
        KeyCode::Enter if m.intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT) => {
            vec![b'\n']
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if m.contains(KeyModifiers::CONTROL) {
                vec![0x08]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return Some(csi_letter('A', m, app_cursor)),
        KeyCode::Down => return Some(csi_letter('B', m, app_cursor)),
        KeyCode::Right => return Some(csi_letter('C', m, app_cursor)),
        KeyCode::Left => return Some(csi_letter('D', m, app_cursor)),
        KeyCode::Home => return Some(csi_letter('H', m, app_cursor)),
        KeyCode::End => return Some(csi_letter('F', m, app_cursor)),
        KeyCode::Insert => return Some(csi_tilde(2, m)),
        KeyCode::Delete => return Some(csi_tilde(3, m)),
        KeyCode::PageUp => return Some(csi_tilde(5, m)),
        KeyCode::PageDown => return Some(csi_tilde(6, m)),
        KeyCode::F(n) => {
            return Some(match n {
                1..=4 => {
                    let l = ['P', 'Q', 'R', 'S'][n as usize - 1];
                    let p = modifier_param(m);
                    if p > 1 {
                        format!("\x1b[1;{p}{l}").into_bytes()
                    } else {
                        format!("\x1bO{l}").into_bytes()
                    }
                }
                5 => csi_tilde(15, m),
                6 => csi_tilde(17, m),
                7 => csi_tilde(18, m),
                8 => csi_tilde(19, m),
                9 => csi_tilde(20, m),
                10 => csi_tilde(21, m),
                11 => csi_tilde(23, m),
                12 => csi_tilde(24, m),
                _ => return None,
            });
        }
        _ => return None,
    };
    if alt {
        out.insert(0, 0x1b);
    }
    Some(out)
}

/// 把文字包成 OSC 52「設定剪貼簿」序列，交給外層終端機（Windows Terminal、iTerm2 等）。
pub fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// 主區域的滑鼠事件，座標已轉成相對於畫面左上角（0 起算）。
pub fn encode_mouse(
    ev: &MouseEvent,
    x: u16,
    y: u16,
    sgr: bool,
    report_motion: bool,
    report_drag: bool,
) -> Option<Vec<u8>> {
    let (mut code, release) = match ev.kind {
        MouseEventKind::Down(b) => (button(b), false),
        MouseEventKind::Up(b) => (button(b), true),
        MouseEventKind::Drag(b) => {
            if !(report_drag || report_motion) {
                return None;
            }
            (button(b) + 32, false)
        }
        MouseEventKind::Moved => {
            if !report_motion {
                return None;
            }
            (35, false)
        }
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    if sgr {
        let c = if release { 'm' } else { 'M' };
        Some(format!("\x1b[<{code};{};{}{c}", x + 1, y + 1).into_bytes())
    } else {
        let code = if release { 3 } else { code };
        let enc = |v: u16| (v + 33).min(255) as u8;
        Some(vec![
            0x1b,
            b'[',
            b'M',
            (code + 32).min(255) as u8,
            enc(x),
            enc(y),
        ])
    }
}

fn button(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn basic_keys() {
        assert_eq!(
            encode_key(&key(KeyCode::Char('中'), KeyModifiers::NONE), false).unwrap(),
            "中".as_bytes()
        );
        assert_eq!(
            encode_key(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap(),
            vec![3]
        );
        assert_eq!(
            encode_key(&key(KeyCode::Char('x'), KeyModifiers::ALT), false).unwrap(),
            b"\x1bx"
        );
        assert_eq!(
            encode_key(&key(KeyCode::Up, KeyModifiers::NONE), false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            encode_key(&key(KeyCode::Up, KeyModifiers::NONE), true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            encode_key(&key(KeyCode::Right, KeyModifiers::CONTROL), true).unwrap(),
            b"\x1b[1;5C"
        );
        assert_eq!(
            encode_key(&key(KeyCode::Delete, KeyModifiers::NONE), false).unwrap(),
            b"\x1b[3~"
        );
        assert_eq!(
            encode_key(&key(KeyCode::Enter, KeyModifiers::NONE), false).unwrap(),
            b"\r"
        );
    }

    #[test]
    fn modified_enter_inserts_newline() {
        let enc = |m| encode_key(&key(KeyCode::Enter, m), false).unwrap();
        assert_eq!(enc(KeyModifiers::NONE), b"\r");
        assert_eq!(enc(KeyModifiers::CONTROL), b"\n");
        assert_eq!(enc(KeyModifiers::SHIFT), b"\n");
        assert_eq!(enc(KeyModifiers::ALT), b"\x1b\r");
    }

    #[test]
    fn prefix() {
        let p = Prefix::parse("ctrl-a");
        assert!(p.matches(&key(KeyCode::Char('a'), KeyModifiers::CONTROL)));
        assert!(!p.matches(&key(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert_eq!(p.byte(), 1);
        assert_eq!(Prefix::parse("ctrl-b").byte(), 2);
    }

    #[test]
    fn osc52_encoding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64("中文".as_bytes()), "5Lit5paH");
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn mouse_sgr() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            encode_mouse(&ev, 4, 2, true, false, false).unwrap(),
            b"\x1b[<0;5;3M"
        );
    }
}
