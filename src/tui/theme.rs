//! Colours and small formatting helpers shared by the TUI's widgets. Normal text
//! keeps the terminal's own colour; everything else is a semantic colour here.

use ratatui::style::Color;

/// Secondary text: project names, meta lines, ages.
pub const MUTED: Color = Color::Rgb(125, 130, 150);
/// Dimmed text (working sessions) and rule lines.
pub const FAINT: Color = Color::Rgb(85, 89, 106);
/// Selection bar and focus.
pub const ACCENT: Color = Color::Rgb(140, 150, 255);
pub const SELECTED_BG: Color = Color::Rgb(46, 48, 64);
pub const WORKING: Color = Color::Rgb(100, 160, 240);
pub const INPUT: Color = Color::Rgb(240, 180, 80);
pub const UNREAD: Color = Color::Rgb(90, 210, 150);
pub const ERROR: Color = Color::Rgb(235, 105, 105);
/// A limit's window used slower than its time goes, about as fast, and
/// faster or used up (`▲` and `✕` tell those apart): saturated, and blue
/// against red, so they're told apart with deuteranopia too.
pub const PACE_SLOW: Color = Color::Rgb(60, 160, 255);
pub const PACE_EVEN: Color = Color::Rgb(150, 150, 150);
pub const PACE_FAST: Color = Color::Rgb(255, 32, 32);

/// Badge colours; a project gets one by a hash of its name.
const PROJECT_COLORS: [Color; 12] = [
    Color::Rgb(52, 211, 153),
    Color::Rgb(96, 165, 250),
    Color::Rgb(244, 114, 182),
    Color::Rgb(251, 191, 36),
    Color::Rgb(167, 139, 250),
    Color::Rgb(45, 212, 191),
    Color::Rgb(251, 146, 60),
    Color::Rgb(129, 140, 248),
    Color::Rgb(163, 230, 53),
    Color::Rgb(248, 113, 113),
    Color::Rgb(56, 189, 248),
    Color::Rgb(232, 121, 249),
];

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How often the spinner moves; the TUI redraws this often while one shows.
pub const SPINNER_MS: i64 = 120;

/// Two letters for a project: the initials of its first two words, else its first
/// two characters ("agent-roster" → "AR", "roster" → "RO").
pub fn monogram(name: &str) -> String {
    let words: Vec<&str> = name
        .split(['-', '_', ' ', '.', '/'])
        .filter(|w| !w.is_empty())
        .collect();
    let letters: String = if words.len() >= 2 {
        words
            .iter()
            .take(2)
            .filter_map(|w| w.chars().next())
            .collect()
    } else {
        name.chars()
            .filter(|c| c.is_alphanumeric())
            .take(2)
            .collect()
    };
    format!("{letters:<2}").to_uppercase()
}

pub fn project_color(name: &str) -> Color {
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    PROJECT_COLORS[(hash % PROJECT_COLORS.len() as u64) as usize]
}

/// The spinner frame for this moment; every spinner on screen moves together.
pub fn spinner(now_ms: i64) -> &'static str {
    SPINNER[((now_ms / SPINNER_MS) % SPINNER.len() as i64) as usize]
}

pub use crate::util::{ago, clock};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monograms() {
        assert_eq!(monogram("agent-roster"), "AR");
        assert_eq!(monogram("roster"), "RO");
        assert_eq!(monogram("v2api"), "V2");
        assert_eq!(monogram("x"), "X ");
    }

    #[test]
    fn project_colors_are_stable() {
        assert_eq!(project_color("roster"), project_color("roster"));
    }
}
