//! 從原始輸出中擷取 OSC 9 / 777 通知與 BEL。
//! `alacritty_terminal` 不會回報這些序列，所以在餵給模擬器之前先掃一遍。

use crate::adapter::OscEvent;

const MAX_OSC: usize = 4096;

#[derive(Default)]
pub struct OscScanner {
    state: State,
    buf: Vec<u8>,
}

#[derive(Default, PartialEq)]
enum State {
    #[default]
    Ground,
    Esc,
    Osc,
    OscEsc,
}

impl OscScanner {
    pub fn feed(&mut self, data: &[u8]) -> Vec<OscEvent> {
        let mut out = Vec::new();
        for &b in data {
            match self.state {
                State::Ground => match b {
                    0x1b => self.state = State::Esc,
                    0x07 => out.push(OscEvent::Bell),
                    _ => {}
                },
                State::Esc => {
                    if b == b']' {
                        self.state = State::Osc;
                        self.buf.clear();
                    } else if b == 0x1b {
                        self.state = State::Esc;
                    } else {
                        self.state = State::Ground;
                    }
                }
                State::Osc => match b {
                    0x07 => {
                        self.finish(&mut out);
                    }
                    0x1b => self.state = State::OscEsc,
                    _ => {
                        if self.buf.len() < MAX_OSC {
                            self.buf.push(b);
                        } else {
                            self.state = State::Ground;
                        }
                    }
                },
                State::OscEsc => {
                    if b == b'\\' {
                        self.finish(&mut out);
                    } else {
                        // 不是 ST，丟棄這個 OSC
                        self.state = if b == 0x1b { State::Esc } else { State::Ground };
                    }
                }
            }
        }
        out
    }

    fn finish(&mut self, out: &mut Vec<OscEvent>) {
        self.state = State::Ground;
        let s = String::from_utf8_lossy(&self.buf).into_owned();
        self.buf.clear();
        if let Some(ev) = parse_osc(&s) {
            out.push(ev);
        }
    }
}

fn parse_osc(s: &str) -> Option<OscEvent> {
    let (code, rest) = s.split_once(';').unwrap_or((s, ""));
    match code {
        "0" | "2" => Some(OscEvent::Title(rest.to_string())),
        "9" => {
            // OSC 9;4;… 是 ConEmu / Windows Terminal 的進度列，不是通知
            if rest.starts_with("4;") || rest == "4" {
                return None;
            }
            Some(OscEvent::Notify {
                title: None,
                body: rest.to_string(),
            })
        }
        "777" => {
            let mut parts = rest.splitn(3, ';');
            if parts.next()? != "notify" {
                return None;
            }
            let title = parts.next().map(str::to_string);
            let body = parts.next().unwrap_or_default().to_string();
            Some(OscEvent::Notify { title, body })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_and_777_across_chunks() {
        let mut s = OscScanner::default();
        let mut ev = s.feed(b"abc\x1b]9;hel");
        ev.extend(s.feed(b"lo\x07x\x1b]777;notify;T;B\x1b\\\x07"));
        assert_eq!(
            ev,
            vec![
                OscEvent::Notify {
                    title: None,
                    body: "hello".into()
                },
                OscEvent::Notify {
                    title: Some("T".into()),
                    body: "B".into()
                },
                OscEvent::Bell,
            ]
        );
    }

    #[test]
    fn progress_is_ignored() {
        let mut s = OscScanner::default();
        assert!(s.feed(b"\x1b]9;4;1;50\x07").is_empty());
    }
}
