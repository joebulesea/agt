//! 分頁之間的檔案衝突預警：同一個 repo 裡兩個分頁動了同一個檔案，在收尾之前就提醒。

use super::*;
use std::collections::BTreeSet;

/// 一個分頁的比對資料。
pub(super) struct Row {
    pub id: String,
    pub short_id: String,
    pub task: String,
    pub repo: String,
    pub worktree: Option<String>,
    pub files: BTreeSet<String>,
}

/// 找出每個分頁與其他分頁重疊的檔案。同一個 repo、不同 worktree 才比對
/// （`--share` 的分頁共用同一個 worktree，改的本來就是同一批檔案）。
pub(super) fn compute(rows: &[Row]) -> HashMap<String, Vec<Overlap>> {
    let mut out: HashMap<String, Vec<Overlap>> = HashMap::new();
    for a in rows {
        for b in rows {
            if a.id == b.id || !agt_git::same_path(Path::new(&a.repo), Path::new(&b.repo)) {
                continue;
            }
            if let (Some(x), Some(y)) = (&a.worktree, &b.worktree)
                && agt_git::same_path(Path::new(x), Path::new(y))
            {
                continue;
            }
            let files: Vec<String> = a.files.intersection(&b.files).cloned().collect();
            if files.is_empty() {
                continue;
            }
            out.entry(a.id.clone()).or_default().push(Overlap {
                short_id: b.short_id.clone(),
                task: b.task.clone(),
                total: files.len(),
                files: files.into_iter().take(Overlap::MAX_FILES).collect(),
            });
        }
    }
    for v in out.values_mut() {
        v.sort_by(|x, y| x.short_id.cmp(&y.short_id));
    }
    out
}

impl Daemon {
    /// 記下分頁目前動過的檔案（輪詢 git 時呼叫）。
    pub(super) fn note_changed_files(&self, id: &str, files: BTreeSet<String>) {
        self.changed_files
            .lock_or_recover()
            .insert(id.to_string(), files);
    }

    /// 依最新的檔案清單重新比對，有變化的分頁推播完整的 `SessionUpdated`。
    pub(super) fn refresh_overlaps(&self) {
        let files = self.changed_files.lock_or_recover().clone();
        let updated: Vec<SessionInfo> = {
            let mut entries = self.entries.lock_or_recover();
            let rows: Vec<Row> = entries
                .values()
                .filter_map(|e| {
                    Some(Row {
                        id: e.info.id.clone(),
                        short_id: e.info.short_id.clone(),
                        task: e.info.task.clone(),
                        repo: e.info.repo_path.clone()?,
                        worktree: e.info.worktree.clone(),
                        files: files.get(&e.info.id)?.clone(),
                    })
                })
                .collect();
            let mut found = compute(&rows);
            entries
                .values_mut()
                .filter_map(|e| {
                    let now = found.remove(&e.info.id).unwrap_or_default();
                    (e.info.overlaps != now).then(|| {
                        e.info.overlaps = now;
                        e.info.clone()
                    })
                })
                .collect()
        };
        for session in updated {
            let _ = self.events.send(ServerMsg::SessionUpdated {
                session: Box::new(session),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, repo: &str, wt: &str, files: &[&str]) -> Row {
        Row {
            id: id.into(),
            short_id: id.into(),
            task: format!("任務 {id}"),
            repo: repo.into(),
            worktree: Some(wt.into()),
            files: files.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn same_file_in_same_repo_overlaps_both_ways() {
        let rows = [
            row("a", "/r", "/w/a", &["src/x.rs", "only-a"]),
            row("b", "/r", "/w/b", &["src/x.rs", "only-b"]),
        ];
        let o = compute(&rows);
        assert_eq!(o["a"].len(), 1);
        assert_eq!(o["a"][0].short_id, "b");
        assert_eq!(o["a"][0].files, vec!["src/x.rs"]);
        assert_eq!(o["b"][0].short_id, "a");
    }

    #[test]
    fn different_repo_or_no_common_file_is_ignored() {
        let rows = [
            row("a", "/r1", "/w/a", &["x"]),
            row("b", "/r2", "/w/b", &["x"]),
            row("c", "/r1", "/w/c", &["y"]),
        ];
        assert!(compute(&rows).is_empty());
    }

    #[test]
    fn shared_worktree_is_not_a_conflict() {
        let rows = [
            row("a", "/r", "/w/same", &["x"]),
            row("b", "/r", "/w/same", &["x"]),
        ];
        assert!(compute(&rows).is_empty());
    }

    #[test]
    fn counts_total_but_caps_listed_files() {
        let many: Vec<String> = (0..30).map(|i| format!("f{i:02}")).collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let rows = [row("a", "/r", "/w/a", &refs), row("b", "/r", "/w/b", &refs)];
        let o = compute(&rows);
        assert_eq!(o["a"][0].total, 30);
        assert_eq!(o["a"][0].files.len(), Overlap::MAX_FILES);
    }

    #[test]
    fn three_way_overlap_lists_each_other_sorted() {
        let rows = [
            row("c", "/r", "/w/c", &["x"]),
            row("a", "/r", "/w/a", &["x"]),
            row("b", "/r", "/w/b", &["x"]),
        ];
        let o = compute(&rows);
        let ids: Vec<&str> = o["c"].iter().map(|v| v.short_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"]);
    }
}
