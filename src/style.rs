//! Terminal styling helpers.
//!
//! Uses `owo-colors` for ANSI output, gated by `supports-color` so color is
//! applied only when stdout is a terminal that supports it. Honors the
//! standard `NO_COLOR` convention implicitly (supports-color checks it).

use std::fmt::Display;
use std::sync::OnceLock;

use owo_colors::{OwoColorize, Style};

fn use_color() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        supports_color::on(supports_color::Stream::Stdout)
            .map(|_| true)
            .unwrap_or(false)
    })
}

/// Format `value` with `style` when the terminal supports color, otherwise
/// return it unstyled.
pub fn styled<D: Display>(value: D, style: Style) -> String {
    if use_color() {
        format!("{}", value.style(style))
    } else {
        format!("{value}")
    }
}

pub fn level_error() -> Style {
    Style::new().red().bold()
}

pub fn level_warn() -> Style {
    Style::new().yellow().bold()
}

pub fn level_info() -> Style {
    Style::new().cyan()
}

pub fn level_ok() -> Style {
    Style::new().green().bold()
}

pub fn dim() -> Style {
    Style::new().dimmed()
}

pub fn bold() -> Style {
    Style::new().bold()
}

pub fn accent() -> Style {
    Style::new().cyan().bold()
}

pub fn muted() -> Style {
    Style::new().bright_black()
}
