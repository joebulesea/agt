//! 主題：左側欄、狀態列與浮動視窗用到的顏色。
//!
//! 欄位以「原本寫死的顏色」命名（`cyan` 是強調色、`yellow` 是警示…），預設的 `dark`
//! 與先前的配色完全相同。分頁中 Agent 自己輸出的顏色不受主題影響。狀態一律另外搭配符號，
//! 不只靠顏色區分。

use ratatui::style::Color;
use std::collections::BTreeMap;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// 強調色：選取的項目、浮動視窗邊框、搜尋與思考中
    pub cyan: Color,
    /// 警示、快捷鍵與等待輸入
    pub yellow: Color,
    /// 成功、執行中
    pub green: Color,
    /// 錯誤
    pub red: Color,
    /// 啟動中
    pub blue: Color,
    pub magenta: Color,
    /// 次要文字
    pub gray: Color,
    /// 淡化文字與邊框
    pub dark_gray: Color,
    /// 一般文字
    pub text: Color,
    /// 彩色背景（強調色、警示色、灰色按鈕）上的文字
    pub on_accent: Color,
    /// 選取列背景
    pub sel_bg: Color,
    /// 底部狀態列背景
    pub bar_bg: Color,
    /// 按鈕：可用
    pub tab_on_bg: Color,
    /// 按鈕：不可用
    pub tab_off_fg: Color,
    pub tab_off_bg: Color,
}

pub const NAMES: [&str; 3] = ["dark", "light", "colorblind"];

impl Theme {
    pub const DARK: Theme = Theme {
        cyan: Color::Cyan,
        yellow: Color::Yellow,
        green: Color::Green,
        red: Color::Red,
        blue: Color::Blue,
        magenta: Color::Magenta,
        gray: Color::Gray,
        dark_gray: Color::DarkGray,
        text: Color::White,
        on_accent: Color::Black,
        sel_bg: Color::Indexed(236),
        bar_bg: Color::Indexed(235),
        tab_on_bg: Color::Indexed(37),
        tab_off_fg: Color::Indexed(244),
        tab_off_bg: Color::Indexed(238),
    };

    /// 亮色背景的終端機：顏色改用較深的色階，彩色背景上的文字改為白色。
    pub const LIGHT: Theme = Theme {
        cyan: Color::Indexed(30),
        yellow: Color::Indexed(130),
        green: Color::Indexed(28),
        red: Color::Indexed(160),
        blue: Color::Indexed(26),
        magenta: Color::Indexed(90),
        gray: Color::Indexed(240),
        dark_gray: Color::Indexed(245),
        text: Color::Black,
        on_accent: Color::White,
        sel_bg: Color::Indexed(254),
        bar_bg: Color::Indexed(253),
        tab_on_bg: Color::Indexed(30),
        tab_off_fg: Color::Indexed(240),
        tab_off_bg: Color::Indexed(252),
    };

    /// 色覺友善（紅綠色盲）：避開紅／綠對比，改用橘與藍（Okabe-Ito 色系）。
    pub const COLORBLIND: Theme = Theme {
        green: Color::Indexed(33),
        red: Color::Indexed(208),
        blue: Color::Indexed(141),
        ..Theme::DARK
    };

    /// 以 `[colors]` 的設定覆寫個別顏色；不認得的名稱或寫法錯誤的值略過（daemon 已驗證過）。
    fn with_overrides(mut self, colors: &BTreeMap<String, String>) -> Theme {
        let tc = crate::ui::truecolor();
        for (k, v) in colors {
            let Ok(c) = agt_proto::colors::parse_color(v) else {
                continue;
            };
            let c = crate::ui::color(c, tc);
            let slot = match k.as_str() {
                "cyan" => &mut self.cyan,
                "yellow" => &mut self.yellow,
                "green" => &mut self.green,
                "red" => &mut self.red,
                "blue" => &mut self.blue,
                "magenta" => &mut self.magenta,
                "gray" => &mut self.gray,
                "dark_gray" => &mut self.dark_gray,
                "text" => &mut self.text,
                "on_accent" => &mut self.on_accent,
                "sel_bg" => &mut self.sel_bg,
                "bar_bg" => &mut self.bar_bg,
                "tab_on_bg" => &mut self.tab_on_bg,
                "tab_off_fg" => &mut self.tab_off_fg,
                "tab_off_bg" => &mut self.tab_off_bg,
                _ => continue,
            };
            *slot = c;
        }
        self
    }

    pub fn by_name(name: &str) -> Option<Theme> {
        match name {
            "dark" => Some(Theme::DARK),
            "light" => Some(Theme::LIGHT),
            "colorblind" => Some(Theme::COLORBLIND),
            _ => None,
        }
    }
}

static CURRENT: RwLock<Theme> = RwLock::new(Theme::DARK);

/// 目前的主題。
pub fn theme() -> Theme {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

/// 切換主題並套用 `[colors]` 覆寫；名稱不認得時改用 `dark`。
pub fn set(name: &str, colors: &BTreeMap<String, String>) {
    let t = Theme::by_name(name)
        .unwrap_or(Theme::DARK)
        .with_overrides(colors);
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = t;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_resolves() {
        for n in NAMES {
            assert!(Theme::by_name(n).is_some(), "{n}");
        }
        assert!(Theme::by_name("nope").is_none());
    }

    #[test]
    fn dark_matches_original_palette() {
        assert_eq!(Theme::DARK.cyan, Color::Cyan);
        assert_eq!(Theme::DARK.on_accent, Color::Black);
        assert_eq!(Theme::DARK.sel_bg, Color::Indexed(236));
    }

    #[test]
    fn colorblind_avoids_red_green() {
        let t = Theme::COLORBLIND;
        assert_ne!(t.green, Color::Green);
        assert_ne!(t.red, Color::Red);
        assert_ne!(t.green, t.red);
        // 其餘沿用 dark
        assert_eq!(t.cyan, Theme::DARK.cyan);
    }

    #[test]
    fn overrides_replace_only_named_colors() {
        let mut m = BTreeMap::new();
        m.insert("cyan".to_string(), "5".to_string());
        m.insert("bogus".to_string(), "1".to_string());
        m.insert("red".to_string(), "not-a-color".to_string());
        let t = Theme::LIGHT.with_overrides(&m);
        assert_eq!(t.cyan, Color::Indexed(5));
        assert_eq!(t.red, Theme::LIGHT.red);
        assert_eq!(t.yellow, Theme::LIGHT.yellow);
    }

    #[test]
    fn light_inverts_text_on_accent() {
        assert_eq!(Theme::LIGHT.text, Color::Black);
        assert_eq!(Theme::LIGHT.on_accent, Color::White);
    }
}
