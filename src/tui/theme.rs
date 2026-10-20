//! Colours and small formatting helpers shared by the TUI's widgets. Normal text
//! keeps the terminal's own colour; everything else is a semantic colour here.

use ratatui::style::Color;

/// Secondary text: project names, meta lines, ages.
pub const MUTED: Color = Color::Rgb(125, 130, 150);
/// Dimmed text (working sessions) and rule lines.
pub const FAINT: Color = Color::Rgb(85, 89, 106);
/// Selection bar and focus.
pub const ACCENT: Color = Color::Rgb(140, 150, 255);
/// Background of the selected row.
pub const SELECTED_BG: Color = Color::Rgb(46, 48, 64);
pub const WORKING: Color = Color::Rgb(100, 160, 240);
pub const INPUT: Color = Color::Rgb(240, 180, 80);
pub const UNREAD: Color = Color::Rgb(90, 210, 150);
pub const ERROR: Color = Color::Rgb(235, 105, 105);
/// Claude's brand orange, for its glyph.
pub const CLAUDE: Color = Color::Rgb(217, 119, 87);

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

/// How long ago, briefly: `now`, `5m`, `3h`, `2d`.
pub fn ago(elapsed_ms: i64) -> String {
    let secs = elapsed_ms.max(0) / 1000;
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// A running clock: `12s`, `5m`, `1h 2m`.
pub fn clock(elapsed_ms: i64) -> String {
    let secs = elapsed_ms.max(0) / 1000;
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

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

    #[test]
    fn times() {
        assert_eq!(ago(30_000), "now");
        assert_eq!(ago(5 * 60_000), "5m");
        assert_eq!(ago(3 * 3_600_000), "3h");
        assert_eq!(ago(50 * 3_600_000), "2d");
        assert_eq!(clock(12_000), "12s");
        assert_eq!(clock(62 * 60_000), "1h 2m");
    }
}
