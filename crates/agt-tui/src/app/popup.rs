//! 對話框的按鍵處理。

use super::*;

impl App {
    pub(super) fn on_popup_key(&mut self, k: KeyEvent) {
        let Some(mut popup) = self.popup.take() else {
            return;
        };
        let esc = k.code == KeyCode::Esc;
        let keep = match &mut popup {
            Popup::Help => {
                !(esc
                    || matches!(
                        k.code,
                        KeyCode::Char('q') | KeyCode::Char('?') | KeyCode::Enter
                    ))
            }
            Popup::Text {
                lines,
                scroll,
                diff,
                tabs,
                tab,
                ..
            } => {
                let n = tabs.len();
                let switch = match k.code {
                    KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') if n > 1 => {
                        Some((*tab + 1) % n)
                    }
                    KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') if n > 1 => {
                        Some((*tab + n - 1) % n)
                    }
                    KeyCode::Char(c @ '1'..='9') if (c as usize - '1' as usize) < n => {
                        Some(c as usize - '1' as usize)
                    }
                    _ => None,
                };
                if let Some(t) = switch {
                    *tab = t;
                    *lines = tabs[t].1.clone();
                    *scroll = 0;
                    self.popup = Some(popup);
                    return;
                }
                let page = self.main_area.height.saturating_sub(4).max(1) as usize;
                let max = crate::ui::text_rows(lines, *diff, self.screen_area.width)
                    .len()
                    .saturating_sub(1);
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => false,
                    KeyCode::Down | KeyCode::Char('j') => {
                        *scroll = (*scroll + 1).min(max);
                        true
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        *scroll = scroll.saturating_sub(1);
                        true
                    }
                    KeyCode::PageDown | KeyCode::Char(' ') => {
                        *scroll = (*scroll + page).min(max);
                        true
                    }
                    KeyCode::PageUp => {
                        *scroll = scroll.saturating_sub(page);
                        true
                    }
                    KeyCode::Home | KeyCode::Char('g') => {
                        *scroll = 0;
                        true
                    }
                    KeyCode::End | KeyCode::Char('G') => {
                        *scroll = max;
                        true
                    }
                    // 複製全部內容（例如收尾失敗的錯誤訊息）：TUI 開了滑鼠模式，無法直接用滑鼠選取
                    KeyCode::Char('c') | KeyCode::Char('y') => {
                        let text = lines.join("\n");
                        self.set_message(format!("已複製 {} 行到剪貼簿", lines.len()), false);
                        self.clipboard_out = Some(text);
                        true
                    }
                    _ => true,
                }
            }
            Popup::Rename { input } => match k.code {
                KeyCode::Esc => false,
                KeyCode::Enter => {
                    if let Some(id) = self.active.clone() {
                        let task = input.clone();
                        self.simple(ClientMsg::Rename { id, task }, "已重新命名");
                    }
                    false
                }
                _ => {
                    edit_text(input, &k);
                    true
                }
            },
            Popup::Finish { selected, .. } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => false,
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = (*selected + FINISH_ITEMS.len() - 1) % FINISH_ITEMS.len();
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1) % FINISH_ITEMS.len();
                    true
                }
                KeyCode::Enter => {
                    let sel = *selected;
                    self.choose_finish(sel);
                    return;
                }
                KeyCode::Char(c) => {
                    if let Some(i) = FINISH_ITEMS.iter().position(|(n, _)| n.starts_with(c)) {
                        self.choose_finish(i);
                        return;
                    }
                    true
                }
                _ => true,
            },
            Popup::Close { id, selected, .. } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('n') => false,
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1) % CLOSE_ITEMS.len();
                    true
                }
                KeyCode::Enter => {
                    let (id, sel) = (id.clone(), *selected);
                    self.choose_close(id, sel);
                    return;
                }
                KeyCode::Char(c) => {
                    if let Some(i) = CLOSE_ITEMS.iter().position(|(n, _)| n.starts_with(c)) {
                        let id = id.clone();
                        self.choose_close(id, i);
                        return;
                    }
                    true
                }
                _ => true,
            },
            Popup::Strategy { selected } => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => false,
                KeyCode::Up | KeyCode::Char('k') => {
                    *selected = (*selected + STRATEGIES.len() - 1) % STRATEGIES.len();
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1) % STRATEGIES.len();
                    true
                }
                KeyCode::Enter => {
                    let s = STRATEGIES[*selected];
                    self.finish(FinishAction::Merge { strategy: Some(s) }, false);
                    false
                }
                _ => true,
            },
            Popup::Confirm { .. } => match k.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Popup::Confirm { action, .. } = popup {
                        match action {
                            Confirm::Close { id } => self.close(id),
                            Confirm::Clipboard(text) => {
                                let n = text.chars().count();
                                self.clipboard_out = Some(text);
                                self.set_message(format!("已複製到剪貼簿（{n} 字）"), false);
                            }
                            Confirm::Finish { id, action } => self.finish_id(id, action, true),
                            Confirm::TrustSetup(mut req) => {
                                req.trust_setup = true;
                                self.create(req);
                            }
                            Confirm::CleanLeftovers => self.clean_leftovers(),
                        }
                    }
                    return;
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc | KeyCode::Char('q') => {
                    false
                }
                _ => true,
            },
            Popup::Switch { input, selected } => {
                let n = self.switch_matches(input).len();
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                match k.code {
                    KeyCode::Esc => false,
                    KeyCode::Enter => {
                        let id = self
                            .switch_matches(input)
                            .get((*selected).min(n.saturating_sub(1)))
                            .map(|s| s.id.clone());
                        match id {
                            Some(id) => {
                                self.select(Some(id));
                                false
                            }
                            None => true,
                        }
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        *selected = (*selected).min(n.saturating_sub(1));
                        *selected = (*selected + n.max(1) - 1) % n.max(1);
                        true
                    }
                    KeyCode::Char('p') if ctrl => {
                        *selected = (*selected).min(n.saturating_sub(1));
                        *selected = (*selected + n.max(1) - 1) % n.max(1);
                        true
                    }
                    KeyCode::Down | KeyCode::Tab => {
                        *selected = (*selected + 1) % n.max(1);
                        true
                    }
                    KeyCode::Char('n') if ctrl => {
                        *selected = (*selected + 1) % n.max(1);
                        true
                    }
                    _ => {
                        edit_text(input, &k);
                        *selected = 0;
                        true
                    }
                }
            }
            Popup::Tags { input } => match k.code {
                KeyCode::Esc => false,
                KeyCode::Enter => {
                    if let Some(id) = self.active.clone() {
                        let tags = agt_proto::tags::split_input(input);
                        self.simple(ClientMsg::SetTags { id, tags }, "已更新標籤");
                    }
                    false
                }
                _ => {
                    edit_text(input, &k);
                    true
                }
            },
            Popup::New(form) if form.browser.is_some() => {
                form.browser_key(&k);
                true
            }
            Popup::New(form) => match k.code {
                KeyCode::Esc => false,
                // Ctrl+O 或 F2：開啟目錄瀏覽器選擇 Repo
                KeyCode::Char('o') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.open_browser(&self.cwd);
                    true
                }
                KeyCode::F(2) => {
                    form.open_browser(&self.cwd);
                    true
                }
                // F3 或 Ctrl+T：切換範本
                KeyCode::F(3) => {
                    form.next_template();
                    true
                }
                KeyCode::Char('t') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    form.next_template();
                    true
                }
                KeyCode::Tab | KeyCode::Down => {
                    form.field = (form.field + 1) % 4;
                    true
                }
                KeyCode::BackTab | KeyCode::Up => {
                    form.field = (form.field + 3) % 4;
                    true
                }
                KeyCode::Left if form.field == 0 => {
                    form.agent = (form.agent + form.agents.len() - 1) % form.agents.len().max(1);
                    true
                }
                KeyCode::Right | KeyCode::Char(' ') if form.field == 0 => {
                    form.agent = (form.agent + 1) % form.agents.len().max(1);
                    true
                }
                KeyCode::Enter => {
                    let req = CreateSession {
                        agent: form
                            .agents
                            .get(form.agent)
                            .cloned()
                            .unwrap_or_else(|| "shell".into()),
                        repo: expand_home(form.repo.trim()),
                        task: form.task.trim().to_string(),
                        base_branch: Some(form.base.trim().to_string()).filter(|s| !s.is_empty()),
                        share: None,
                        trust_setup: false,
                        cols: Some(self.main_area.width.max(20)),
                        rows: Some(self.main_area.height.max(5)),
                        template: form.template_name().map(str::to_string),
                        tags: vec![],
                    };
                    self.create(req);
                    false
                }
                _ => {
                    match form.field {
                        1 => edit_text(&mut form.repo, &k),
                        2 => edit_text(&mut form.task, &k),
                        3 => edit_text(&mut form.base, &k),
                        _ => {}
                    }
                    true
                }
            },
        };
        if keep {
            self.popup = Some(popup);
        }
    }

    pub(super) fn choose_close(&mut self, id: String, i: usize) {
        if CLOSE_ITEMS[i].0 == "delete" {
            // 與收尾的 discard 相同：有未 commit / 未合併的內容時會再確認一次
            self.finish_id(id, FinishAction::Discard, false);
        } else {
            self.close(id);
        }
    }

    pub(super) fn choose_finish(&mut self, i: usize) {
        match FINISH_ITEMS[i].0 {
            "merge" => {
                if self.ui.merge_strategy == "ask" {
                    self.popup = Some(Popup::Strategy { selected: 0 });
                } else {
                    self.finish(FinishAction::Merge { strategy: None }, false);
                }
            }
            "pr" => self.finish(FinishAction::Pr, false),
            "keep" => self.finish(FinishAction::Keep, false),
            _ => self.finish(FinishAction::Discard, false),
        }
    }
}
