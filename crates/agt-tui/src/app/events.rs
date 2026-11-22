//! daemon 事件與非同步結果的處理。

use super::*;

impl App {
    // ───────────────────────── daemon 事件 ─────────────────────────

    pub fn on_incoming(&mut self, inc: Incoming) {
        match inc {
            Incoming::Screen(u) => {
                if Some(u.id()) != self.active.as_deref() {
                    return;
                }
                match u {
                    ScreenUpdate::Snapshot { screen, .. } => self.screen = Some(screen),
                    ScreenUpdate::Diff { diff, .. } => {
                        if let Some(s) = self.screen.as_mut() {
                            s.apply(&diff);
                        }
                    }
                }
                self.dirty = true;
            }
            Incoming::Msg(m) => self.on_msg(*m),
            Incoming::Closed => {
                self.quit = true;
                self.set_message("與 daemon 的連線已中斷", true);
            }
        }
    }

    pub(super) fn on_msg(&mut self, m: ServerMsg) {
        match m {
            ServerMsg::SessionUpdated { session } => self.upsert(*session),
            ServerMsg::SessionRemoved { id } => {
                let order: Vec<String> = self.ordered().iter().map(|s| s.id.clone()).collect();
                let pos = order.iter().position(|x| *x == id);
                self.sessions.retain(|s| s.id != id);
                if self.active.as_deref() == Some(&id) {
                    self.active = None;
                    self.screen = None;
                    let order: Vec<String> = self.ordered().iter().map(|s| s.id.clone()).collect();
                    let next =
                        pos.and_then(|p| order.get(p.min(order.len().saturating_sub(1))).cloned());
                    self.select(next);
                }
            }
            ServerMsg::StatusChanged { id, to, .. } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.status = to;
                }
            }
            ServerMsg::WorktreeChanged {
                id,
                ahead,
                behind,
                dirty,
            } => {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
                    s.ahead = ahead;
                    s.behind = behind;
                    s.dirty = dirty;
                }
            }
            ServerMsg::ConfigStatus { error, ui } => {
                if let Some(ui) = ui {
                    self.prefix = Prefix::parse(&ui.prefix_key);
                    crate::theme::set(&ui.theme, &ui.colors);
                    self.ui = ui;
                    self.sent_size = None;
                }
                self.config_error = error;
            }
            ServerMsg::Clipboard { id, text } => {
                // 只接受目前分頁的複製要求，背景分頁不能偷偷改剪貼簿
                if self.active.as_deref() == Some(id.as_str()) {
                    let n = text.chars().count();
                    match self.ui.osc52.as_str() {
                        "on" => {
                            self.clipboard_out = Some(text);
                            self.set_message(format!("已複製到剪貼簿（{n} 字）"), false);
                        }
                        "off" => {}
                        // ask：顯示內容讓使用者確認。內容可能是 Agent 想讓你貼到終端機的指令
                        _ if self.popup.is_none() => {
                            const PREVIEW: usize = 400;
                            let shown: String = text.chars().take(PREVIEW).collect();
                            let more = if n > PREVIEW {
                                format!("\n…（只顯示前 {PREVIEW} 字；按 y 會複製全部 {n} 字）")
                            } else {
                                String::new()
                            };
                            let warn = if agt_proto::text::has_controls(&text) {
                                "\n注意：內容含有控制字元（已顯示為符號）。"
                            } else {
                                ""
                            };
                            self.popup = Some(Popup::Confirm {
                                text: format!(
                                    "分頁要求把以下內容複製到剪貼簿（{n} 字）：\n\n{}{more}\n{warn}\n要複製嗎？",
                                    agt_proto::text::printable(&shown)
                                ),
                                action: Confirm::Clipboard(text),
                            });
                        }
                        // 已有其他對話框：不打斷，直接拒絕
                        _ => self.set_message("已拒絕分頁的剪貼簿寫入（有其他對話框）", true),
                    }
                }
            }
            ServerMsg::DaemonStopping => {
                self.quit = true;
            }
            ServerMsg::Error { message, .. } => self.set_message(message, true),
            ServerMsg::Reply { .. } => {}
        }
        self.dirty = true;
    }

    pub fn on_app_event(&mut self, ev: AppEvent) {
        self.dirty = true;
        match ev {
            AppEvent::Created(info) => {
                let info = *info;
                self.message = None;
                let id = info.id.clone();
                self.upsert(info);
                self.select(Some(id));
            }
            AppEvent::CreateFailed(b) => {
                let (e, req) = *b;
                self.message = None;
                if e.code() == Some(codes::SETUP_UNTRUSTED) {
                    let cmd = match &e {
                        ClientError::Remote { message, .. } => message.clone(),
                        _ => String::new(),
                    };
                    self.popup = Some(Popup::Confirm {
                        text: format!(
                            "這個 repo 設定了 setup 指令，第一次執行前需要確認：\n\n  {cmd}\n\n要執行嗎？(y/n)"
                        ),
                        action: Confirm::TrustSetup(req),
                    });
                } else {
                    self.show_text("無法建立分頁", &e.to_string(), false);
                }
            }
            AppEvent::Finished(action, Ok(text)) => {
                if !text.is_empty() && !matches!(action, FinishAction::Keep) {
                    self.show_text(&format!("{} 完成", action.name()), &text, false);
                } else {
                    self.set_message(text, false);
                }
            }
            AppEvent::Finished(action, Err(e)) => match (e.code(), &action) {
                (Some(codes::NEEDS_CONFIRM), _) => {
                    if let Some(id) = self.active.clone() {
                        self.popup = Some(Popup::Confirm {
                            text: format!("{e}\n\n(y/n)"),
                            action: Confirm::Finish {
                                id,
                                action: action.clone(),
                            },
                        });
                    }
                }
                (Some(codes::NEED_STRATEGY), _) => {
                    self.popup = Some(Popup::Strategy { selected: 0 })
                }
                _ => self.show_text(&format!("{} 失敗", action.name()), &e.to_string(), false),
            },
            AppEvent::Diff(Ok(text)) => self.show_text("diff", &text, true),
            AppEvent::FinishPreview(id, action, r) => {
                if self.active.as_deref() == Some(id.as_str())
                    && let Some(Popup::Finish { preview, .. }) = self.popup.as_mut()
                {
                    preview[usize::from(action == "pr")] = match r {
                        // 預覽內容來自 Agent 寫的說明檔：控制字元換成可見符號，避免寫進終端機
                        Ok(t) => t
                            .lines()
                            .map(|l| {
                                l.chars()
                                    .map(|c| if c.is_control() { '·' } else { c })
                                    .collect()
                            })
                            .collect(),
                        Err(e) => vec![format!("無法預覽：{e}")],
                    };
                }
            }
            AppEvent::Diff(Err(e)) => self.set_message(e.to_string(), true),
            AppEvent::Synced(Ok(text)) => self.set_message(text, false),
            // 衝突時訊息含檔案清單，單行訊息放不下
            AppEvent::Synced(Err(e)) => self.show_text("同步失敗", &e.to_string(), false),
            AppEvent::Searched(found) => {
                if !found {
                    self.set_message("找不到符合的內容", true);
                }
            }
            AppEvent::Closed(Ok(())) => {}
            AppEvent::Leftovers(items) => {
                if !items.is_empty() && self.popup.is_none() {
                    self.popup = Some(Popup::Confirm {
                        text: leftovers_text(&items),
                        action: Confirm::CleanLeftovers,
                    });
                }
            }
            AppEvent::Closed(Err(e)) => self.set_message(e.to_string(), true),
            AppEvent::Error(m) => self.set_message(m, true),
            AppEvent::Info(m) => self.set_message(m, false),
        }
    }

    pub(super) fn show_text(&mut self, title: &str, text: &str, diff: bool) {
        // diff、錯誤訊息可能含有檔案內容裡的控制序列：ratatui 會原樣寫到終端機，先換成可見的符號
        let lines: Vec<String> = agt_proto::text::printable(text)
            .lines()
            .map(|l| l.replace('\t', "    "))
            .collect();
        let mut tabs: Vec<(String, Vec<String>)> = vec![];
        if diff {
            for l in &lines {
                if let Some(name) = l.strip_prefix("### ") {
                    tabs.push((name.to_string(), vec![]));
                } else if let Some((_, body)) = tabs.last_mut() {
                    body.push(l.clone());
                }
            }
        }
        let first = tabs.first().map(|t| t.1.clone());
        self.popup = Some(Popup::Text {
            title: title.into(),
            lines: first.unwrap_or(lines),
            scroll: 0,
            diff,
            tabs,
            tab: 0,
        });
    }
}
