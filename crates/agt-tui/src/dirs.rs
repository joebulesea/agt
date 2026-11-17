//! 新增分頁表單的目錄瀏覽器：選擇 Repo 用。
//!
//! 列出目前目錄下的子目錄（git repo 會標記），可以輸入文字篩選；
//! 第一項「使用這個目錄」選定目前目錄，第二項 `..` 回上一層。
//! Windows 在磁碟根目錄再往上一層會列出所有磁碟。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// 選定目前所在的目錄。
    UseThis,
    /// 上一層。
    Parent,
    Dir,
    /// Windows 的磁碟（例如 `C:\`）。
    #[cfg_attr(not(windows), allow(dead_code))]
    Drive,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    /// 這個目錄是 git repo（含有 `.git`）。
    pub git: bool,
    pub kind: Kind,
}

/// 瀏覽器按下 Enter 後的結果。
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    /// 還在瀏覽（進入了某個目錄）。
    Browsing,
    /// 選定這個目錄。
    Chosen(PathBuf),
}

pub struct DirBrowser {
    /// 目前所在的目錄；None = 磁碟清單（Windows）。
    pub dir: Option<PathBuf>,
    entries: Vec<Entry>,
    pub filter: String,
    pub selected: usize,
    pub error: Option<String>,
}

fn is_git(p: &Path) -> bool {
    p.join(".git").exists()
}

#[cfg(windows)]
fn drives() -> Vec<Entry> {
    (b'A'..=b'Z')
        .map(|c| format!("{}:\\", c as char))
        .filter(|d| Path::new(d).exists())
        .map(|d| Entry {
            name: d.clone(),
            path: PathBuf::from(&d),
            git: false,
            kind: Kind::Drive,
        })
        .collect()
}

#[cfg(not(windows))]
fn drives() -> Vec<Entry> {
    vec![]
}

impl DirBrowser {
    /// 從 `start` 開始瀏覽；不存在時往上找到存在的目錄，再不行用 `fallback`。
    pub fn open(start: &str, fallback: &str) -> DirBrowser {
        let mut p = PathBuf::from(start.trim());
        while !p.is_dir() {
            match p.parent() {
                Some(up) if !up.as_os_str().is_empty() => p = up.to_path_buf(),
                _ => {
                    p = PathBuf::from(fallback);
                    break;
                }
            }
        }
        let mut b = DirBrowser {
            dir: None,
            entries: vec![],
            filter: String::new(),
            selected: 0,
            error: None,
        };
        b.load(Some(p));
        b
    }

    /// 切換到某個目錄（None = 磁碟清單）並重新讀取內容。
    pub fn load(&mut self, dir: Option<PathBuf>) {
        self.filter.clear();
        self.selected = 0;
        self.error = None;
        let mut entries = vec![];
        match &dir {
            None => entries = drives(),
            Some(d) => {
                entries.push(Entry {
                    name: "使用這個目錄".into(),
                    path: d.clone(),
                    git: is_git(d),
                    kind: Kind::UseThis,
                });
                let parent = d.parent().map(Path::to_path_buf);
                if parent.is_some() || cfg!(windows) {
                    entries.push(Entry {
                        name: "..".into(),
                        path: parent.clone().unwrap_or_default(),
                        git: false,
                        kind: Kind::Parent,
                    });
                }
                match std::fs::read_dir(d) {
                    Ok(rd) => {
                        let mut dirs: Vec<Entry> = rd
                            .filter_map(|e| e.ok())
                            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                            .filter_map(|e| {
                                let name = e.file_name().to_string_lossy().into_owned();
                                (!name.starts_with('.') && !name.starts_with('$')).then(|| {
                                    let path = e.path();
                                    Entry {
                                        git: is_git(&path),
                                        name,
                                        path,
                                        kind: Kind::Dir,
                                    }
                                })
                            })
                            .collect();
                        dirs.sort_by_key(|e| e.name.to_lowercase());
                        entries.extend(dirs);
                    }
                    Err(e) => self.error = Some(format!("無法讀取：{e}")),
                }
            }
        }
        self.dir = dir;
        self.entries = entries;
    }

    /// 目前顯示的項目（套用篩選；「使用這個目錄」與 `..` 永遠顯示）。
    pub fn visible(&self) -> Vec<&Entry> {
        let f = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| {
                matches!(e.kind, Kind::UseThis | Kind::Parent)
                    || f.is_empty()
                    || e.name.to_lowercase().contains(&f)
            })
            .collect()
    }

    pub fn move_by(&mut self, delta: i32) {
        let n = self.visible().len() as i32;
        if n > 0 {
            self.selected = (self.selected as i32 + delta).clamp(0, n - 1) as usize;
        }
    }

    pub fn set_filter(&mut self, f: String) {
        self.filter = f;
        // 篩選後跳到第一個符合的子目錄
        let first = self
            .visible()
            .iter()
            .position(|e| matches!(e.kind, Kind::Dir | Kind::Drive));
        self.selected = if self.filter.is_empty() {
            0
        } else {
            first.unwrap_or(0)
        };
    }

    pub fn up(&mut self) {
        if let Some(d) = &self.dir {
            let parent = d.parent().map(Path::to_path_buf);
            if parent.is_some() || cfg!(windows) {
                let from = d.clone();
                self.load(parent);
                // 回到上一層時，游標停在剛剛所在的目錄
                if let Some(i) = self.visible().iter().position(|e| e.path == from) {
                    self.selected = i;
                }
            }
        }
    }

    /// 對目前選到的項目按 Enter。
    pub fn enter(&mut self) -> Pick {
        let Some(e) = self.visible().get(self.selected).map(|e| (*e).clone()) else {
            return Pick::Browsing;
        };
        match e.kind {
            Kind::UseThis => Pick::Chosen(e.path),
            Kind::Parent => {
                self.up();
                Pick::Browsing
            }
            Kind::Dir | Kind::Drive => {
                self.load(Some(e.path));
                Pick::Browsing
            }
        }
    }

    /// 直接選定目前反白的目錄（不進入）；「..」不能選。
    pub fn choose_selected(&self) -> Option<PathBuf> {
        let e = self.visible().get(self.selected).copied()?;
        (e.kind != Kind::Parent).then(|| e.path.clone())
    }

    /// 標題列顯示的目前位置。
    pub fn title(&self) -> String {
        match &self.dir {
            Some(d) => d.display().to_string(),
            None => "磁碟".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for p in ["beta", "Alpha/.git", ".hidden", "gamma/sub"] {
            std::fs::create_dir_all(d.path().join(p)).unwrap();
        }
        std::fs::write(d.path().join("file.txt"), "x").unwrap();
        d
    }

    fn names(b: &DirBrowser) -> Vec<String> {
        b.visible().iter().map(|e| e.name.clone()).collect()
    }

    #[test]
    fn lists_dirs_sorted_and_marks_git() {
        let d = tree();
        let b = DirBrowser::open(&d.path().to_string_lossy(), ".");
        assert_eq!(names(&b), ["使用這個目錄", "..", "Alpha", "beta", "gamma"]);
        assert!(b.visible()[2].git, "Alpha 有 .git");
        assert!(!b.visible()[3].git);
    }

    #[test]
    fn navigate_filter_and_choose() {
        let d = tree();
        let mut b = DirBrowser::open(&d.path().to_string_lossy(), ".");
        b.set_filter("GAM".into());
        assert_eq!(names(&b), ["使用這個目錄", "..", "gamma"]);
        assert_eq!(b.selected, 2, "篩選後跳到第一個符合的目錄");
        assert_eq!(b.enter(), Pick::Browsing);
        assert_eq!(b.dir.as_deref(), Some(d.path().join("gamma").as_path()));
        assert!(b.filter.is_empty());
        // 回上一層，游標停在 gamma
        b.up();
        assert_eq!(b.visible()[b.selected].name, "gamma");
        // 直接選定反白的目錄
        assert_eq!(b.choose_selected(), Some(d.path().join("gamma")));
        // 第一項：使用這個目錄
        b.selected = 0;
        assert_eq!(b.enter(), Pick::Chosen(d.path().to_path_buf()));
    }

    #[test]
    fn missing_start_falls_back_to_existing_parent() {
        let d = tree();
        let b = DirBrowser::open(&d.path().join("nope/deeper").to_string_lossy(), ".");
        assert_eq!(b.dir.as_deref(), Some(d.path()));
    }
}
