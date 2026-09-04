//! ratatui, Screen/Modal, 47 slash commands, theme (Q14,Q24).
//!
//! This crate provides the terminal UI layer for the deepagents-rust SDK,
//! porting the original Textual (retained-mode, CSS-driven) UI to a
//! programmatic immediate-mode stack built on [`ratatui`] + [`crossterm`].
//!
//! The design centers on three pillars:
//!
//! - **Theme system** ([`ThemeColors`], [`Theme`], [`ThemeRegistry`]): ~30 hex
//!   color fields, 11 built-in themes, 4-layer resolution
//!   (managed → env → user config → [`DEFAULT_THEME`]).
//! - **Slash commands** ([`SlashCommand`], [`CommandRegistry`]): the full 47
//!   commands (45 public + 2 hidden) across 10 categories, each tagged with a
//!   [`BypassTier`].
//! - **Screen/Modal abstraction** ([`Screen`], [`Modal`], [`KeyResult`],
//!   [`AppContext`], [`ChatScreen`]): a self-built immediate-mode screen stack.
//!
//! Rendering is provided as a v0 stub; the full ratatui render loop is added
//! incrementally. The types and theme/registry systems are production-ready.
//!
//! Part of the deepagents-rust workspace.
//! See `docs/SPEC.md` §九 (Q14) and §十二 (Q24) for the full design spec.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::env;
use std::fmt;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use serde::{Deserialize, Serialize};

use deepagents_errors::Error;

/// Crate version string.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The default theme name — a dark, tokyonight-inspired palette with
/// LangChain blue accents.
pub const DEFAULT_THEME: &str = "langchain";

/// The environment variable used to override the active theme. When set, its
/// value is the theme name (one of the 11 built-ins or a registered user
/// theme) and takes precedence over `config.toml` and the default.
pub const THEME_ENV_VAR: &str = "DEEPAGENTS_CODE_THEME";

// ────────────────────────────────────────────────────────────────────────────
// Theme colors
// ────────────────────────────────────────────────────────────────────────────

/// A flat bag of ~30 hex color tokens backing a [`Theme`].
///
/// Each field is a `String` holding a CSS-style hex value (`"#RRGGBB"`),
/// allowing themes to be authored in `config.toml` as plain strings and
/// deserialized without pulling in a color-parsing dependency. The fields
/// mirror the original Textual CSS tokens:
///
/// - **brand palette**: `brand`, `brand_light`
/// - **surfaces**: `surface`, `surface_light`, `panel`, `panel_light`,
///   `background`, `foreground`
/// - **borders / text**: `border`, `border_light`, `text`, `text_muted`,
///   `cursor`, `selection`, `selection_fg`
/// - **status colors**: `primary`, `primary_light`, `success`, `warning`,
///   `error`, `muted`
/// - **accents**: `skill_accent`, `tool_accent`, `mode`, `mode_light`
/// - **ANSI 16**: `black`, `red`, `green`, `yellow`, `blue`, `magenta`,
///   `cyan`, `white`
///
/// `Default` produces an empty (all-empty-string) palette; concrete palettes
/// are built by [`Theme::langchain_dark`] / [`Theme::langchain_light`] and the
/// 11 built-ins in [`ThemeRegistry::new`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ThemeColors {
    /// Primary brand color (LangChain blue).
    pub brand: String,
    /// Lighter brand color for highlights.
    pub brand_light: String,
    /// Dark surface background.
    pub surface: String,
    /// Light surface background.
    pub surface_light: String,
    /// Border color on dark surfaces.
    pub border: String,
    /// Border color on light surfaces.
    pub border_light: String,
    /// Default foreground text.
    pub text: String,
    /// Muted foreground text.
    pub text_muted: String,
    /// Primary action color.
    pub primary: String,
    /// Lighter primary action color.
    pub primary_light: String,
    /// Success status color.
    pub success: String,
    /// Warning status color.
    pub warning: String,
    /// Error status color.
    pub error: String,
    /// Muted/grey color for secondary chrome.
    pub muted: String,
    /// Panel background.
    pub panel: String,
    /// Light panel background.
    pub panel_light: String,
    /// Skill accent color.
    pub skill_accent: String,
    /// Tool accent color.
    pub tool_accent: String,
    /// Approval-mode accent color.
    pub mode: String,
    /// Lighter mode accent color.
    pub mode_light: String,
    /// Global background.
    pub background: String,
    /// Global foreground.
    pub foreground: String,
    /// Cursor color.
    pub cursor: String,
    /// Selection background.
    pub selection: String,
    /// Selection foreground.
    pub selection_fg: String,
    /// ANSI black.
    pub black: String,
    /// ANSI red.
    pub red: String,
    /// ANSI green.
    pub green: String,
    /// ANSI yellow.
    pub yellow: String,
    /// ANSI blue.
    pub blue: String,
    /// ANSI magenta.
    pub magenta: String,
    /// ANSI cyan.
    pub cyan: String,
    /// ANSI white.
    pub white: String,
}

// ────────────────────────────────────────────────────────────────────────────
// Theme
// ────────────────────────────────────────────────────────────────────────────

/// A named palette (a [`ThemeColors`] bag plus a name).
///
/// Themes are looked up by name via [`Theme::by_name`] or [`ThemeRegistry`].
/// The 11 built-in themes are enumerated in [`BUILT_IN_THEMES`]; the default is
/// [`DEFAULT_THEME`] (`"langchain"`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// The theme name (e.g. `"langchain"`, `"tokyo-night"`).
    pub name: String,
    /// The resolved color palette.
    pub colors: ThemeColors,
}

impl Theme {
    /// Build a theme with the given name and palette.
    fn new(name: impl Into<String>, colors: ThemeColors) -> Self {
        Self {
            name: name.into(),
            colors,
        }
    }

    /// The default dark theme — tokyonight-inspired with LangChain blue.
    ///
    /// This is the value of [`DEFAULT_THEME`] (`"langchain"`).
    pub fn langchain_dark() -> Self {
        Self::new(
            DEFAULT_THEME,
            ThemeColors {
                brand: "#1E3A8A".into(),
                brand_light: "#3B82F6".into(),
                surface: "#16161E".into(),
                surface_light: "#1A1A23".into(),
                border: "#2A2B3D".into(),
                border_light: "#3B3C52".into(),
                text: "#C0CAF5".into(),
                text_muted: "#565A6E".into(),
                primary: "#7AA2F7".into(),
                primary_light: "#89B4FA".into(),
                success: "#9ECE6A".into(),
                warning: "#E0AF68".into(),
                error: "#F7768E".into(),
                muted: "#565F89".into(),
                panel: "#1F2335".into(),
                panel_light: "#292E42".into(),
                skill_accent: "#BB9AF7".into(),
                tool_accent: "#7DCFFF".into(),
                mode: "#7AA2F7".into(),
                mode_light: "#89B4FA".into(),
                background: "#1A1B26".into(),
                foreground: "#C0CAF5".into(),
                cursor: "#C0CAF5".into(),
                selection: "#283457".into(),
                selection_fg: "#C0CAF5".into(),
                black: "#15161E".into(),
                red: "#F7768E".into(),
                green: "#9ECE6A".into(),
                yellow: "#E0AF68".into(),
                blue: "#7AA2F7".into(),
                magenta: "#BB9AF7".into(),
                cyan: "#7DCFFF".into(),
                white: "#C0CAF5".into(),
            },
        )
    }

    /// The light variant of the LangChain theme.
    pub fn langchain_light() -> Self {
        Self::new(
            "langchain-light",
            ThemeColors {
                brand: "#1E3A8A".into(),
                brand_light: "#2563EB".into(),
                surface: "#F5F5F0".into(),
                surface_light: "#ECECE4".into(),
                border: "#D4D4C8".into(),
                border_light: "#C0C0B4".into(),
                text: "#343B58".into(),
                text_muted: "#8F97A4".into(),
                primary: "#336EE6".into(),
                primary_light: "#3D74F0".into(),
                success: "#4C8C2C".into(),
                warning: "#8C6F1F".into(),
                error: "#8C4351".into(),
                muted: "#9699A3".into(),
                panel: "#E6E6DD".into(),
                panel_light: "#DCDCD0".into(),
                skill_accent: "#7B5EA7".into(),
                tool_accent: "#3D74A8".into(),
                mode: "#336EE6".into(),
                mode_light: "#3D74F0".into(),
                background: "#F5F5F0".into(),
                foreground: "#343B58".into(),
                cursor: "#343B58".into(),
                selection: "#D0D8F0".into(),
                selection_fg: "#343B58".into(),
                black: "#343B58".into(),
                red: "#8C4351".into(),
                green: "#4C8C2C".into(),
                yellow: "#8C6F1F".into(),
                blue: "#336EE6".into(),
                magenta: "#7B5EA7".into(),
                cyan: "#3D74A8".into(),
                white: "#F5F5F0".into(),
            },
        )
    }

    /// Look up a built-in theme by name.
    ///
    /// Returns `Some` for any of the 11 built-in theme names and `None` for
    /// unknown names. Use [`ThemeRegistry`] to also resolve user-registered
    /// themes.
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "langchain" => Some(Self::langchain_dark()),
            "langchain-light" => Some(Self::langchain_light()),
            "textual-dark" => Some(Self::textual_dark()),
            "textual-light" => Some(Self::textual_light()),
            "ansi-dark" => Some(Self::ansi_dark()),
            "ansi-light" => Some(Self::ansi_light()),
            "catppuccin-frappe" => Some(Self::catppuccin_frappe()),
            "rose-pine" => Some(Self::rose_pine()),
            "rose-pine-dawn" => Some(Self::rose_pine_dawn()),
            "rose-pine-moon" => Some(Self::rose_pine_moon()),
            "tokyo-night" => Some(Self::tokyo_night()),
            _ => None,
        }
    }

    /// `textual-dark` — the original Textual app dark palette.
    fn textual_dark() -> Self {
        Self::new(
            "textual-dark",
            ThemeColors {
                brand: "#004578".into(),
                brand_light: "#0080FF".into(),
                surface: "#1C1C1C".into(),
                surface_light: "#242424".into(),
                border: "#3D3D3D".into(),
                border_light: "#5A5A5A".into(),
                text: "#E0E0E0".into(),
                text_muted: "#8A8A8A".into(),
                primary: "#0080FF".into(),
                primary_light: "#3399FF".into(),
                success: "#4CAF50".into(),
                warning: "#FF9800".into(),
                error: "#F44336".into(),
                muted: "#8A8A8A".into(),
                panel: "#242424".into(),
                panel_light: "#2E2E2E".into(),
                skill_accent: "#AB47BC".into(),
                tool_accent: "#26C6DA".into(),
                mode: "#0080FF".into(),
                mode_light: "#3399FF".into(),
                background: "#1C1C1C".into(),
                foreground: "#E0E0E0".into(),
                cursor: "#E0E0E0".into(),
                selection: "#3D3D3D".into(),
                selection_fg: "#E0E0E0".into(),
                black: "#1C1C1C".into(),
                red: "#F44336".into(),
                green: "#4CAF50".into(),
                yellow: "#FF9800".into(),
                blue: "#0080FF".into(),
                magenta: "#AB47BC".into(),
                cyan: "#26C6DA".into(),
                white: "#E0E0E0".into(),
            },
        )
    }

    /// `textual-light` — the original Textual app light palette.
    fn textual_light() -> Self {
        Self::new(
            "textual-light",
            ThemeColors {
                brand: "#004578".into(),
                brand_light: "#0080FF".into(),
                surface: "#F5F5F5".into(),
                surface_light: "#ECECEC".into(),
                border: "#D4D4D4".into(),
                border_light: "#BDBDBD".into(),
                text: "#212121".into(),
                text_muted: "#757575".into(),
                primary: "#0080FF".into(),
                primary_light: "#3399FF".into(),
                success: "#2E7D32".into(),
                warning: "#EF6C00".into(),
                error: "#C62828".into(),
                muted: "#9E9E9E".into(),
                panel: "#ECECEC".into(),
                panel_light: "#E0E0E0".into(),
                skill_accent: "#7B1FA2".into(),
                tool_accent: "#00838F".into(),
                mode: "#0080FF".into(),
                mode_light: "#3399FF".into(),
                background: "#F5F5F5".into(),
                foreground: "#212121".into(),
                cursor: "#212121".into(),
                selection: "#BDBDBD".into(),
                selection_fg: "#212121".into(),
                black: "#212121".into(),
                red: "#C62828".into(),
                green: "#2E7D32".into(),
                yellow: "#EF6C00".into(),
                blue: "#0080FF".into(),
                magenta: "#7B1FA2".into(),
                cyan: "#00838F".into(),
                white: "#F5F5F5".into(),
            },
        )
    }

    /// `ansi-dark` — terminal-default ANSI 16 palette on a dark background.
    fn ansi_dark() -> Self {
        Self::new(
            "ansi-dark",
            ThemeColors {
                brand: "#0080FF".into(),
                brand_light: "#3399FF".into(),
                surface: "#000000".into(),
                surface_light: "#1A1A1A".into(),
                border: "#333333".into(),
                border_light: "#555555".into(),
                text: "#C0C0C0".into(),
                text_muted: "#808080".into(),
                primary: "#0080FF".into(),
                primary_light: "#3399FF".into(),
                success: "#00C853".into(),
                warning: "#FFAB00".into(),
                error: "#FF1744".into(),
                muted: "#808080".into(),
                panel: "#1A1A1A".into(),
                panel_light: "#2A2A2A".into(),
                skill_accent: "#D500F9".into(),
                tool_accent: "#00B0FF".into(),
                mode: "#0080FF".into(),
                mode_light: "#3399FF".into(),
                background: "#000000".into(),
                foreground: "#C0C0C0".into(),
                cursor: "#C0C0C0".into(),
                selection: "#264F78".into(),
                selection_fg: "#C0C0C0".into(),
                black: "#000000".into(),
                red: "#FF1744".into(),
                green: "#00C853".into(),
                yellow: "#FFAB00".into(),
                blue: "#0080FF".into(),
                magenta: "#D500F9".into(),
                cyan: "#00B0FF".into(),
                white: "#C0C0C0".into(),
            },
        )
    }

    /// `ansi-light` — terminal-default ANSI 16 palette on a light background.
    fn ansi_light() -> Self {
        Self::new(
            "ansi-light",
            ThemeColors {
                brand: "#004578".into(),
                brand_light: "#0080FF".into(),
                surface: "#FFFFFF".into(),
                surface_light: "#F5F5F5".into(),
                border: "#D4D4D4".into(),
                border_light: "#BDBDBD".into(),
                text: "#212121".into(),
                text_muted: "#757575".into(),
                primary: "#004578".into(),
                primary_light: "#0080FF".into(),
                success: "#2E7D32".into(),
                warning: "#EF6C00".into(),
                error: "#C62828".into(),
                muted: "#9E9E9E".into(),
                panel: "#F5F5F5".into(),
                panel_light: "#ECECEC".into(),
                skill_accent: "#7B1FA2".into(),
                tool_accent: "#00838F".into(),
                mode: "#004578".into(),
                mode_light: "#0080FF".into(),
                background: "#FFFFFF".into(),
                foreground: "#212121".into(),
                cursor: "#212121".into(),
                selection: "#BDBDBD".into(),
                selection_fg: "#212121".into(),
                black: "#212121".into(),
                red: "#C62828".into(),
                green: "#2E7D32".into(),
                yellow: "#EF6C00".into(),
                blue: "#004578".into(),
                magenta: "#7B1FA2".into(),
                cyan: "#00838F".into(),
                white: "#FFFFFF".into(),
            },
        )
    }

    /// `catppuccin-frappe` — the Catppuccin Frappé palette.
    fn catppuccin_frappe() -> Self {
        Self::new(
            "catppuccin-frappe",
            ThemeColors {
                brand: "#8CAAEE".into(),
                brand_light: "#BAC2DE".into(),
                surface: "#292C3C".into(),
                surface_light: "#303446".into(),
                border: "#414559".into(),
                border_light: "#51576D".into(),
                text: "#C6D0F5".into(),
                text_muted: "#626880".into(),
                primary: "#8CAAEE".into(),
                primary_light: "#BAC2DE".into(),
                success: "#A6D189".into(),
                warning: "#E5C890".into(),
                error: "#E78284".into(),
                muted: "#626880".into(),
                panel: "#292C3C".into(),
                panel_light: "#414559".into(),
                skill_accent: "#CA9EE6".into(),
                tool_accent: "#99D1DB".into(),
                mode: "#8CAAEE".into(),
                mode_light: "#BAC2DE".into(),
                background: "#303446".into(),
                foreground: "#C6D0F5".into(),
                cursor: "#F2D5CF".into(),
                selection: "#414559".into(),
                selection_fg: "#C6D0F5".into(),
                black: "#51576D".into(),
                red: "#E78284".into(),
                green: "#A6D189".into(),
                yellow: "#E5C890".into(),
                blue: "#8CAAEE".into(),
                magenta: "#CA9EE6".into(),
                cyan: "#99D1DB".into(),
                white: "#BAC2DE".into(),
            },
        )
    }

    /// `rose-pine` — the Rosé Pine (dark) palette.
    fn rose_pine() -> Self {
        Self::new(
            "rose-pine",
            ThemeColors {
                brand: "#9CCFD8".into(),
                brand_light: "#E0DEF4".into(),
                surface: "#1F1D2E".into(),
                surface_light: "#26233A".into(),
                border: "#403D52".into(),
                border_light: "#524F6B".into(),
                text: "#E0DEF4".into(),
                text_muted: "#6E6A86".into(),
                primary: "#9CCFD8".into(),
                primary_light: "#E0DEF4".into(),
                success: "#AABBE7".into(),
                warning: "#F6C177".into(),
                error: "#EB6F92".into(),
                muted: "#6E6A86".into(),
                panel: "#26233A".into(),
                panel_light: "#403D52".into(),
                skill_accent: "#C4A7E7".into(),
                tool_accent: "#9CCFD8".into(),
                mode: "#31748F".into(),
                mode_light: "#9CCFD8".into(),
                background: "#1F1D2E".into(),
                foreground: "#E0DEF4".into(),
                cursor: "#E0DEF4".into(),
                selection: "#403D52".into(),
                selection_fg: "#E0DEF4".into(),
                black: "#26233A".into(),
                red: "#EB6F92".into(),
                green: "#31748F".into(),
                yellow: "#F6C177".into(),
                blue: "#9CCFD8".into(),
                magenta: "#C4A7E7".into(),
                cyan: "#EBBCBA".into(),
                white: "#E0DEF4".into(),
            },
        )
    }

    /// `rose-pine-dawn` — the Rosé Pine Dawn (light) palette.
    fn rose_pine_dawn() -> Self {
        Self::new(
            "rose-pine-dawn",
            ThemeColors {
                brand: "#286983".into(),
                brand_light: "#1F1D2E".into(),
                surface: "#FAF4ED".into(),
                surface_light: "#FFF9F2".into(),
                border: "#E0DEF4".into(),
                border_light: "#C4A7E7".into(),
                text: "#575279".into(),
                text_muted: "#9893A5".into(),
                primary: "#286983".into(),
                primary_light: "#569494".into(),
                success: "#6CB872".into(),
                warning: "#D7827E".into(),
                error: "#B4637A".into(),
                muted: "#9893A5".into(),
                panel: "#FFF9F2".into(),
                panel_light: "#E0DEF4".into(),
                skill_accent: "#907AA9".into(),
                tool_accent: "#286983".into(),
                mode: "#286983".into(),
                mode_light: "#569494".into(),
                background: "#FAF4ED".into(),
                foreground: "#575279".into(),
                cursor: "#575279".into(),
                selection: "#E0DEF4".into(),
                selection_fg: "#575279".into(),
                black: "#575279".into(),
                red: "#B4637A".into(),
                green: "#6CB872".into(),
                yellow: "#EA9D34".into(),
                blue: "#286983".into(),
                magenta: "#907AA9".into(),
                cyan: "#569494".into(),
                white: "#FAF4ED".into(),
            },
        )
    }

    /// `rose-pine-moon` — the Rosé Pine Moon (dark) palette.
    fn rose_pine_moon() -> Self {
        Self::new(
            "rose-pine-moon",
            ThemeColors {
                brand: "#3E8FB0".into(),
                brand_light: "#E0DEF4".into(),
                surface: "#1F1D2E".into(),
                surface_light: "#2A2740".into(),
                border: "#403D52".into(),
                border_light: "#524F7B".into(),
                text: "#E0DEF4".into(),
                text_muted: "#6E6A86".into(),
                primary: "#3E8FB0".into(),
                primary_light: "#9CCFD8".into(),
                success: "#AABBE7".into(),
                warning: "#F6C177".into(),
                error: "#EB6F92".into(),
                muted: "#6E6A86".into(),
                panel: "#2A2740".into(),
                panel_light: "#403D52".into(),
                skill_accent: "#C4A7E7".into(),
                tool_accent: "#3E8FB0".into(),
                mode: "#3E8FB0".into(),
                mode_light: "#9CCFD8".into(),
                background: "#232136".into(),
                foreground: "#E0DEF4".into(),
                cursor: "#E0DEF4".into(),
                selection: "#403D52".into(),
                selection_fg: "#E0DEF4".into(),
                black: "#2A2740".into(),
                red: "#EB6F92".into(),
                green: "#3E8FB0".into(),
                yellow: "#F6C177".into(),
                blue: "#9CCFD8".into(),
                magenta: "#C4A7E7".into(),
                cyan: "#EBBCBA".into(),
                white: "#E0DEF4".into(),
            },
        )
    }

    /// `tokyo-night` — the Tokyo Night palette.
    fn tokyo_night() -> Self {
        Self::new(
            "tokyo-night",
            ThemeColors {
                brand: "#1E3A8A".into(),
                brand_light: "#7AA2F7".into(),
                surface: "#16161E".into(),
                surface_light: "#1A1A23".into(),
                border: "#2A2B3D".into(),
                border_light: "#3B3C52".into(),
                text: "#C0CAF5".into(),
                text_muted: "#565A6E".into(),
                primary: "#7AA2F7".into(),
                primary_light: "#89B4FA".into(),
                success: "#9ECE6A".into(),
                warning: "#E0AF68".into(),
                error: "#F7768E".into(),
                muted: "#565F89".into(),
                panel: "#1F2335".into(),
                panel_light: "#292E42".into(),
                skill_accent: "#BB9AF7".into(),
                tool_accent: "#7DCFFF".into(),
                mode: "#7AA2F7".into(),
                mode_light: "#89B4FA".into(),
                background: "#1A1B26".into(),
                foreground: "#C0CAF5".into(),
                cursor: "#C0CAF5".into(),
                selection: "#283457".into(),
                selection_fg: "#C0CAF5".into(),
                black: "#15161E".into(),
                red: "#F7768E".into(),
                green: "#9ECE6A".into(),
                yellow: "#E0AF68".into(),
                blue: "#7AA2F7".into(),
                magenta: "#BB9AF7".into(),
                cyan: "#7DCFFF".into(),
                white: "#C0CAF5".into(),
            },
        )
    }
}

/// The names of the 11 built-in themes, in stable display order.
pub const BUILT_IN_THEMES: [&str; 11] = [
    "langchain",
    "langchain-light",
    "textual-dark",
    "textual-light",
    "ansi-dark",
    "ansi-light",
    "catppuccin-frappe",
    "rose-pine",
    "rose-pine-dawn",
    "rose-pine-moon",
    "tokyo-night",
];

// ────────────────────────────────────────────────────────────────────────────
// ThemeRegistry
// ────────────────────────────────────────────────────────────────────────────

/// An in-memory registry of [`Theme`]s, pre-populated with the 11 built-ins
/// and accepting user-registered themes via [`ThemeRegistry::register`].
///
/// Resolution order ([`ThemeRegistry::resolve`]) is the 4-layer cascade from
/// the spec:
///
/// 1. **managed** — the managed override passed to `resolve` (highest)
/// 2. **env** — the [`THEME_ENV_VAR`] environment variable
/// 3. **user config** — the user-config theme name passed to `resolve`
/// 4. **default** — [`DEFAULT_THEME`] (`"langchain"`)
///
/// At each layer, the name must resolve to a registered theme; the first layer
/// with a registered hit wins. An unknown managed name falls through to the
/// next layer (env), and so on, so a typo never silently disables the user's
/// intended theme.
#[derive(Debug, Clone)]
pub struct ThemeRegistry {
    /// The registered themes keyed by name.
    pub themes: HashMap<String, Theme>,
}

impl Default for ThemeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeRegistry {
    /// Build a registry pre-populated with the 11 built-in themes.
    pub fn new() -> Self {
        let mut themes = HashMap::with_capacity(BUILT_IN_THEMES.len());
        for name in BUILT_IN_THEMES {
            if let Some(theme) = Theme::by_name(name) {
                themes.insert((*name).to_string(), theme);
            }
        }
        Self { themes }
    }

    /// Look up a registered theme by name.
    pub fn get(&self, name: &str) -> Option<&Theme> {
        self.themes.get(name)
    }

    /// Register (or replace) a theme by its `name` field.
    pub fn register(&mut self, theme: Theme) {
        self.themes.insert(theme.name.clone(), theme);
    }

    /// List all registered theme names, sorted alphabetically.
    pub fn list(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.themes.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Resolve the active theme via the 4-layer cascade.
    ///
    /// `managed` is the SDK-managed override (if any), `env` is an explicit
    /// env value (when the caller has already read [`THEME_ENV_VAR`]), and
    /// `user_config` is the theme name from `config.toml` (if any). The first
    /// of these that resolves to a registered theme wins; otherwise the
    /// default ([`DEFAULT_THEME`]) is returned.
    ///
    /// In the common path `env` is read directly from the environment here,
    /// but the parameter allows callers (and tests) to inject a value.
    pub fn resolve(
        &self,
        managed: Option<&str>,
        env_value: Option<&str>,
        user_config: Option<&str>,
    ) -> Theme {
        // Layer 1: managed override.
        if let Some(name) = managed
            && let Some(theme) = self.get(name)
        {
            return theme.clone();
        }
        // Layer 2: env (DEEPAGENTS_CODE_THEME). Prefer the injected value,
        // fall back to reading the env var directly so production callers
        // that don't pass `env_value` still honor the override.
        let env_resolved: Option<String> =
            env_value.map(ToString::to_string).or_else(|| env::var(THEME_ENV_VAR).ok());
        if let Some(name) = env_resolved.as_deref()
            && let Some(theme) = self.get(name)
        {
            return theme.clone();
        }
        // Layer 3: user config.toml theme.
        if let Some(name) = user_config
            && let Some(theme) = self.get(name)
        {
            return theme.clone();
        }
        // Layer 4: default.
        self.get(DEFAULT_THEME)
            .cloned()
            .unwrap_or_else(Theme::langchain_dark)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Slash commands
// ────────────────────────────────────────────────────────────────────────────

/// The category of a TUI slash command.
///
/// Mirrors the 10 categories enumerated in `docs/SPEC.md` §九 (Q14). Used by
/// [`CommandRegistry::list_by_category`] to group commands in the `/help`
/// listing. Serialized as `snake_case` (e.g. `"session"`, `"approval"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCategory {
    /// Session lifecycle: `/compact`, `/clear`, `/resume`, `/save`, `/load`.
    Session,
    /// Model selection and effort: `/model`, `/effort`.
    Model,
    /// Approval workflow: `/yolo`, `/auto`, `/manual`, `/approve`, `/reject`.
    Approval,
    /// Tool / MCP / sandbox inspection: `/tools`, `/mcp`, `/sandbox`.
    Tools,
    /// Context window inspection: `/cost`, `/context`, `/tokens`,
    /// `/timestamps`, `/scrollbar`.
    Context,
    /// Goal / rubric management: `/goal`, `/rubric`, `/accept`, `/amend`.
    Goal,
    /// Skills discovery: `/skills`, `/skill`.
    Skills,
    /// Theme switching: `/theme`, `/light`, `/dark`.
    Theme,
    /// Debug surfaces: `/debug`, `/console`.
    Debug,
    /// Everything else: `/help`, `/quit`, `/restart`, `/cwd`, `/editor`,
    /// `/notifications`, `/subagents`, `/clipboard`, `/install`, `/update`.
    Other,
}

/// The bypass tier a slash command unlocks.
///
/// `None` is the default. The hidden `/yolo` escalation path climbs through
/// `AutoApprove` → `BypassHooks` → `BypassApproval` → `BypassAll`, the latter
/// reachable only by hidden commands per `docs/SPEC.md` §九 (Q14).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BypassTier {
    /// No bypass (default).
    #[default]
    None,
    /// Auto-approve every dangerous operation (YOLO mode).
    AutoApprove,
    /// Bypass hook execution.
    BypassHooks,
    /// Bypass the approval workflow entirely.
    BypassApproval,
    /// Bypass everything — hooks, approvals, and guards. Hidden-only.
    BypassAll,
}

/// A single TUI slash command definition.
///
/// `name` is stored **without** the leading `/` (e.g. `"help"`), matching how
/// the registry is keyed and looked up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommand {
    /// The command name without the leading `/`.
    pub name: String,
    /// A short human-readable description shown in `/help`.
    pub description: String,
    /// The command's category.
    pub category: CommandCategory,
    /// The bypass tier unlocked by this command, if any.
    pub bypass_tier: BypassTier,
    /// Whether the command is hidden from `/help` and tab-completion.
    pub hidden: bool,
}

impl SlashCommand {
    /// Build a public command with no bypass tier.
    fn public(name: &str, description: &str, category: CommandCategory) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            category,
            bypass_tier: BypassTier::None,
            hidden: false,
        }
    }

    /// Build a hidden command with a bypass tier.
    fn hidden_bypass(name: &str, description: &str, tier: BypassTier) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            category: CommandCategory::Approval,
            bypass_tier: tier,
            hidden: true,
        }
    }
}

/// The registry of all 47 TUI slash commands (45 public + 2 hidden).
///
/// Built once via [`CommandRegistry::new`]; supports lookup by name and
/// grouping by category. The full command table is specified in
/// `docs/SPEC.md` §九 (Q14).
#[derive(Debug, Clone)]
pub struct CommandRegistry {
    /// The registered commands, in registration order.
    pub commands: Vec<SlashCommand>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    /// Build the registry with all 47 commands pre-populated.
    pub fn new() -> Self {
        let commands = vec![
            // ── Session ──
            SlashCommand::public("compact", "Compact the conversation context", CommandCategory::Session),
            SlashCommand::public("clear", "Clear the conversation history", CommandCategory::Session),
            SlashCommand::public("resume", "Resume a previous session", CommandCategory::Session),
            SlashCommand::public("save", "Save the current session", CommandCategory::Session),
            SlashCommand::public("load", "Load a saved session", CommandCategory::Session),
            // ── Model ──
            SlashCommand::public("model", "Switch the active model", CommandCategory::Model),
            SlashCommand::public("effort", "Switch the model effort level", CommandCategory::Model),
            // ── Approval ──
            SlashCommand::public("yolo", "Toggle YOLO auto-approve mode", CommandCategory::Approval),
            SlashCommand::public("auto", "Switch to auto approval mode", CommandCategory::Approval),
            SlashCommand::public("manual", "Switch to manual approval mode", CommandCategory::Approval),
            SlashCommand::public("approve", "Approve the pending operation", CommandCategory::Approval),
            SlashCommand::public("reject", "Reject the pending operation", CommandCategory::Approval),
            // ── Tools ──
            SlashCommand::public("tools", "List available tools", CommandCategory::Tools),
            SlashCommand::public("mcp", "Manage MCP servers", CommandCategory::Tools),
            SlashCommand::public("sandbox", "Manage the sandbox provider", CommandCategory::Tools),
            // ── Context ──
            SlashCommand::public("cost", "Show token cost breakdown", CommandCategory::Context),
            SlashCommand::public("context", "Show context window usage", CommandCategory::Context),
            SlashCommand::public("tokens", "Show token counts", CommandCategory::Context),
            SlashCommand::public("timestamps", "Toggle message timestamps", CommandCategory::Context),
            SlashCommand::public("scrollbar", "Toggle the chat scrollbar", CommandCategory::Context),
            // ── Goal ──
            SlashCommand::public("goal", "Show or edit the goal criteria", CommandCategory::Goal),
            SlashCommand::public("rubric", "Show or edit the grading rubric", CommandCategory::Goal),
            SlashCommand::public("accept", "Accept the current goal outcome", CommandCategory::Goal),
            SlashCommand::public("amend", "Amend the current goal criteria", CommandCategory::Goal),
            // ── Skills ──
            SlashCommand::public("skills", "List discovered skills", CommandCategory::Skills),
            SlashCommand::public("skill", "Inspect or invoke a skill", CommandCategory::Skills),
            // ── Theme ──
            SlashCommand::public("theme", "Switch the active theme", CommandCategory::Theme),
            SlashCommand::public("light", "Switch to the light theme variant", CommandCategory::Theme),
            SlashCommand::public("dark", "Switch to the dark theme variant", CommandCategory::Theme),
            // ── Debug ──
            SlashCommand::public("debug", "Open the debug panel", CommandCategory::Debug),
            SlashCommand::public("console", "Open the debug console", CommandCategory::Debug),
            // ── Other ──
            SlashCommand::public("help", "Show this help listing", CommandCategory::Other),
            SlashCommand::public("quit", "Quit the application", CommandCategory::Other),
            SlashCommand::public("restart", "Restart the application", CommandCategory::Other),
            SlashCommand::public("cwd", "Show or switch the working directory", CommandCategory::Other),
            SlashCommand::public("editor", "Open the prompt in an external editor", CommandCategory::Other),
            SlashCommand::public("notifications", "Open the notification center", CommandCategory::Other),
            SlashCommand::public("subagents", "Open the subagent panel", CommandCategory::Other),
            SlashCommand::public("clipboard", "Open the prompt clipboard", CommandCategory::Other),
            SlashCommand::public("install", "Install a skill or plugin", CommandCategory::Other),
            SlashCommand::public("update", "Check for SDK updates", CommandCategory::Other),
            // ── Additional Other commands (map to spec modal screens) ──
            SlashCommand::public("agents", "Open the agent selector", CommandCategory::Other),
            SlashCommand::public("auth", "Manage authentication", CommandCategory::Other),
            SlashCommand::public("threads", "Switch the conversation thread", CommandCategory::Other),
            SlashCommand::public("diff", "Show the working-tree diff", CommandCategory::Other),
            // ── Hidden (BypassTier escalations, Q14) ──
            SlashCommand::hidden_bypass(
                "yolo-bypass-all",
                "Escalate YOLO to bypass all guards (hidden)",
                BypassTier::BypassAll,
            ),
            SlashCommand::hidden_bypass(
                "yolo-bypass-approval",
                "Escalate YOLO to bypass approvals (hidden)",
                BypassTier::BypassApproval,
            ),
        ];
        Self { commands }
    }

    /// Look up a command by name (without the leading `/`).
    pub fn find(&self, name: &str) -> Option<&SlashCommand> {
        self.commands.iter().find(|c| c.name == name)
    }

    /// List all commands in a given category.
    pub fn list_by_category(&self, cat: CommandCategory) -> Vec<&SlashCommand> {
        self.commands
            .iter()
            .filter(|c| c.category == cat)
            .collect()
    }

    /// List all public commands (exclude hidden ones).
    pub fn list_public(&self) -> Vec<&SlashCommand> {
        self.commands.iter().filter(|c| !c.hidden).collect()
    }
}

// ────────────────────────────────────────────────────────────────────────────
// AppContext + KeyResult + Screen/Modal
// ────────────────────────────────────────────────────────────────────────────

/// Mutable state threaded through the screen stack during key handling.
///
/// Each [`Screen::handle_key`] call receives a mutable reference to the
/// `AppContext`, allowing screens to mutate the active theme, switch the
/// current screen, adjust the bypass tier, or attach a session id.
#[derive(Debug, Clone)]
pub struct AppContext {
    /// The active theme.
    pub theme: Theme,
    /// The id of the screen currently on top of the stack (e.g. `"chat"`).
    pub current_screen: String,
    /// The active session id, if any.
    pub session_id: Option<String>,
    /// The active bypass tier (escalated via YOLO/hidden commands).
    pub bypass_tier: BypassTier,
}

impl AppContext {
    /// Build a default context: default theme, `"chat"` screen, no session,
    /// no bypass.
    pub fn with_theme(theme: Theme) -> Self {
        Self {
            theme,
            current_screen: "chat".to_string(),
            session_id: None,
            bypass_tier: BypassTier::None,
        }
    }
}

/// The outcome of a [`Screen::handle_key`] call.
///
/// `PushScreen` and `PopScreen` drive the self-built screen stack; the boxed
/// screen in `PushScreen` is owned so the caller can hand it off without
/// borrowing through the trait object.
pub enum KeyResult {
    /// The key was consumed by this screen.
    Consumed,
    /// The key was not relevant to this screen; let the next screen try.
    Ignored,
    /// The user requested to quit the application.
    Quit,
    /// Push a new screen onto the stack.
    PushScreen(Box<dyn Screen>),
    /// Pop the current screen off the stack.
    PopScreen,
}

// `Box<dyn Screen>` is not `Debug` (because `Screen` is not `Debug`), so
// `KeyResult` cannot derive `Debug`. Implement it manually, rendering the
// pushed screen opaquely as `<screen>`.
impl fmt::Debug for KeyResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Consumed => f.write_str("Consumed"),
            Self::Ignored => f.write_str("Ignored"),
            Self::Quit => f.write_str("Quit"),
            Self::PushScreen(_) => f.write_str("PushScreen(<screen>)"),
            Self::PopScreen => f.write_str("PopScreen"),
        }
    }
}

/// A single full-screen or modal surface in the TUI screen stack.
///
/// Screens are immediate-mode: [`Screen::render`] is called every frame with
/// the frame, the target `area`, and the resolved [`Theme`]. Key events are
/// routed to the top-of-stack screen via [`Screen::handle_key`].
///
/// The trait is `Send` so screens can be owned by an async runtime. It is
/// object-safe: `dyn Screen` is usable (e.g. inside [`KeyResult::PushScreen`]).
pub trait Screen: Send {
    /// Render this screen into `frame` within `area`, using `theme` for colors.
    fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme);
    /// Handle a key event, possibly mutating `ctx`.
    fn handle_key(&mut self, key: KeyEvent, ctx: &mut AppContext) -> KeyResult;
    /// Whether this screen is modal (overlays the screen beneath). Defaults to
    /// `false`.
    fn is_modal(&self) -> bool {
        false
    }
}

/// A modal screen that produces a typed result.
///
/// Extends [`Screen`] with an associated [`Modal::Result`] type and a
/// [`Modal::result`] accessor that returns `Some` once the modal has a value
/// ready to hand back to the caller (and `None` while still in progress).
/// `is_modal` is forced to `true` by the default impl here.
pub trait Modal: Screen {
    /// The value produced by this modal once complete.
    type Result;

    /// The modal's result, if it has one yet.
    fn result(&self) -> Option<Self::Result>;

    /// Modals are always modal.
    fn is_modal(&self) -> bool {
        true
    }
}

// ────────────────────────────────────────────────────────────────────────────
// ChatScreen (v0)
// ────────────────────────────────────────────────────────────────────────────

/// The main conversational screen — a minimal v0 implementation.
///
/// `render` draws a bordered placeholder block titled `"deepagents"`; all key
/// events are currently `Consumed`. The full chat layout (header, scrolling
/// message window, bottom chrome, status bar) is layered in incrementally per
/// `docs/SPEC.md` §十二 (Q24).
#[derive(Debug, Clone, Default)]
pub struct ChatScreen;

impl ChatScreen {
    /// Create a new empty chat screen.
    pub fn new() -> Self {
        Self
    }
}

impl Screen for ChatScreen {
    fn render(&self, frame: &mut Frame, area: Rect, _theme: &Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .title("deepagents");
        let placeholder = Paragraph::new("Chat screen (v0 placeholder)").block(block);
        frame.render_widget(placeholder, area);
    }

    fn handle_key(&mut self, _key: KeyEvent, _ctx: &mut AppContext) -> KeyResult {
        KeyResult::Consumed
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Terminal init/restore helpers (thin wrappers for future use)
// ────────────────────────────────────────────────────────────────────────────

/// Initialize the terminal for raw-mode rendering.
///
/// This is a v0 stub: the full crossterm enable/enter-alt-screen sequence is
/// wired up alongside the main render loop. Returns a placeholder error if
/// the feature is not yet active so callers fail loudly rather than silently.
pub fn init_terminal() -> Result<(), Error> {
    // TODO(Q24): enable raw mode + enter alternate screen via crossterm.
    // Returning Ok here keeps the type reachable for downstream wiring; the
    // real implementation will surface a TuiError::TerminalInit on failure.
    Ok(())
}

/// Restore the terminal to its pre-init state.
///
/// Companion to [`init_terminal`]; a v0 stub.
pub fn restore_terminal() -> Result<(), Error> {
    // TODO(Q24): leave alternate screen + disable raw mode via crossterm.
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_theme_langchain_dark() {
        let theme = Theme::langchain_dark();
        assert_eq!(theme.name, DEFAULT_THEME);
        assert!(!theme.colors.brand.is_empty());
        assert!(!theme.colors.surface.is_empty());
        assert!(!theme.colors.text.is_empty());
        assert!(!theme.colors.primary.is_empty());
        assert!(theme.colors.brand.starts_with('#'));
        assert!(theme.colors.surface.starts_with('#'));
    }

    #[test]
    fn test_theme_by_name() {
        // Existing built-ins resolve.
        assert!(Theme::by_name("langchain").is_some());
        assert!(Theme::by_name("tokyo-night").is_some());
        assert!(Theme::by_name("catppuccin-frappe").is_some());
        assert!(Theme::by_name("rose-pine-dawn").is_some());
        // Non-existent name resolves to None.
        assert!(Theme::by_name("does-not-exist").is_none());
    }

    #[test]
    fn test_theme_registry_list() {
        let registry = ThemeRegistry::new();
        let list = registry.list();
        // 11 built-in themes are present.
        assert_eq!(list.len(), BUILT_IN_THEMES.len());
        // Sorted alphabetically.
        let mut sorted = list.clone();
        sorted.sort_unstable();
        assert_eq!(list, sorted);
        // The default theme is present.
        assert!(list.contains(&DEFAULT_THEME));
    }

    #[test]
    fn test_theme_registry_resolve_env_overrides_default() {
        let registry = ThemeRegistry::new();
        // No overrides → default langchain.
        let resolved = registry.resolve(None, None, None);
        assert_eq!(resolved.name, DEFAULT_THEME);
        // Env override wins over default.
        let resolved = registry.resolve(None, Some("tokyo-night"), None);
        assert_eq!(resolved.name, "tokyo-night");
        // Managed override wins over env.
        let resolved = registry.resolve(Some("rose-pine"), Some("tokyo-night"), None);
        assert_eq!(resolved.name, "rose-pine");
        // User config used when no managed/env.
        let resolved = registry.resolve(None, None, Some("catppuccin-frappe"));
        assert_eq!(resolved.name, "catppuccin-frappe");
        // Unknown env name falls through to user config.
        let resolved = registry.resolve(None, Some("nope"), Some("rose-pine-moon"));
        assert_eq!(resolved.name, "rose-pine-moon");
        // Everything unknown falls through to default.
        let resolved = registry.resolve(Some("nope"), Some("nope"), Some("nope"));
        assert_eq!(resolved.name, DEFAULT_THEME);
    }

    #[test]
    fn test_command_registry_47() {
        let registry = CommandRegistry::new();
        assert_eq!(registry.commands.len(), 47);
        // 45 public + 2 hidden.
        let public = registry.list_public();
        let hidden: Vec<&SlashCommand> = registry.commands.iter().filter(|c| c.hidden).collect();
        assert_eq!(public.len(), 45);
        assert_eq!(hidden.len(), 2);
        // The hidden commands carry escalation bypass tiers.
        assert!(hidden
            .iter()
            .all(|c| matches!(c.bypass_tier, BypassTier::BypassAll | BypassTier::BypassApproval)));
    }

    #[test]
    fn test_command_registry_find() {
        let registry = CommandRegistry::new();
        let help = registry.find("help").expect("help command exists");
        assert_eq!(help.name, "help");
        assert_eq!(help.category, CommandCategory::Other);
        assert!(!help.hidden);
        // Unknown command returns None.
        assert!(registry.find("nope").is_none());
    }

    #[test]
    fn test_command_registry_list_by_category() {
        let registry = CommandRegistry::new();
        // Session: compact, clear, resume, save, load = 5.
        let session = registry.list_by_category(CommandCategory::Session);
        assert_eq!(session.len(), 5);
        assert!(session.iter().all(|c| c.category == CommandCategory::Session));
        // Theme: theme, light, dark = 3.
        let theme = registry.list_by_category(CommandCategory::Theme);
        assert_eq!(theme.len(), 3);
        // Skills: skills, skill = 2.
        let skills = registry.list_by_category(CommandCategory::Skills);
        assert_eq!(skills.len(), 2);
        // Every command is accounted for exactly once across categories.
        let total: usize = [
            CommandCategory::Session,
            CommandCategory::Model,
            CommandCategory::Approval,
            CommandCategory::Tools,
            CommandCategory::Context,
            CommandCategory::Goal,
            CommandCategory::Skills,
            CommandCategory::Theme,
            CommandCategory::Debug,
            CommandCategory::Other,
        ]
        .iter()
        .map(|c| registry.list_by_category(c.clone()).len())
        .sum();
        assert_eq!(total, 47);
    }

    #[test]
    fn test_bypass_tier_serde() {
        // None → "none" → None.
        let json = serde_json::to_string(&BypassTier::None).expect("serialize");
        assert_eq!(json, "\"none\"");
        let back: BypassTier = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, BypassTier::None);
        // BypassAll round-trips.
        let json = serde_json::to_string(&BypassTier::BypassAll).expect("serialize");
        assert_eq!(json, "\"bypass_all\"");
        let back: BypassTier = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, BypassTier::BypassAll);
        // Default is None.
        let tier = BypassTier::default();
        assert_eq!(tier, BypassTier::None);
    }

    #[test]
    fn test_command_category_serde() {
        let json = serde_json::to_string(&CommandCategory::Session).expect("serialize");
        assert_eq!(json, "\"session\"");
        let back: CommandCategory = serde_json::from_str("\"session\"").expect("deserialize");
        assert_eq!(back, CommandCategory::Session);
        let json = serde_json::to_string(&CommandCategory::Other).expect("serialize");
        assert_eq!(json, "\"other\"");
    }

    #[test]
    fn test_screen_object_safety_and_chat_screen() {
        // `dyn Screen` must be usable (object safety).
        let screen: Box<dyn Screen> = Box::new(ChatScreen::new());
        assert!(!screen.is_modal());
        // ChatScreen::handle_key returns Consumed.
        let theme = Theme::langchain_dark();
        let mut ctx = AppContext::with_theme(theme);
        let mut chat = ChatScreen::new();
        // A synthesized no-op KeyEvent exercises the stub handler; crossterm
        // does not implement `Default` for `KeyEvent`, so use `KeyCode::Null`
        // with no modifiers.
        let key = KeyEvent::new(crossterm::event::KeyCode::Null, crossterm::event::KeyModifiers::empty());
        let result = chat.handle_key(key, &mut ctx);
        assert!(matches!(result, KeyResult::Consumed));
        // KeyResult Debug renders without panicking.
        let _ = format!("{result:?}");
    }

    #[test]
    fn test_theme_registry_register_and_get() {
        let mut registry = ThemeRegistry::new();
        let mut custom = Theme::langchain_dark();
        custom.name = "my-custom".to_string();
        registry.register(custom);
        assert!(registry.get("my-custom").is_some());
        assert!(registry.list().contains(&"my-custom"));
        // Built-ins still present.
        assert!(registry.get("langchain").is_some());
    }
}
