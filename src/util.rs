//! Small helpers: time, terminal size, shell quoting, hashing, paths, model
//! names.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Columns and rows of the terminal on stdout, stderr or stdin, if there is one.
pub fn terminal_size() -> Option<(u16, u16)> {
    for fd in [1, 2, 0] {
        // SAFETY: TIOCGWINSZ only writes into the winsize struct we pass.
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0
            && ws.ws_col > 0
            && ws.ws_row > 0
        {
            return Some((ws.ws_col, ws.ws_row));
        }
    }
    None
}

/// Quotes `s` for a POSIX shell.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// FNV-1a hash as hex. Used to notice screen changes across runs, so it has to be
/// stable (std's hasher isn't guaranteed to be).
pub fn fnv1a(data: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Whether `command` would run: an executable file at that path, or (a bare
/// name) in a directory of `$PATH`.
pub fn on_path(command: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let runs = |p: &std::path::Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if command.contains('/') {
        return runs(std::path::Path::new(command));
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| runs(&dir.join(command))))
}

/// Runs `$VISUAL` or `$EDITOR` (else vi) on `file`, in this terminal; whether
/// it exited successfully.
pub fn run_editor(file: &std::path::Path) -> anyhow::Result<bool> {
    let editor = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()))
        .unwrap_or_else(|| "vi".into());
    // Through a shell, so an editor given with options ("code -w") works.
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$1\""))
        .arg("roster-edit")
        .arg(file)
        .status()
        .map(|status| status.success())
        .map_err(|e| anyhow::anyhow!("can't run {editor}: {e}"))
}

/// A model id as Claude shows it: "claude-opus-5-5[1m]" → "Opus 5.5 (1M)",
/// "claude-3-5-sonnet-20241022" → "Sonnet 3.5". Anything else stays as it is.
pub fn model_name(id: &str) -> String {
    let (base, long) = match id.strip_suffix("[1m]") {
        Some(base) => (base, true),
        None => (id, false),
    };
    let Some(rest) = base.strip_prefix("claude-") else {
        return id.to_string();
    };
    let mut family = Vec::new();
    let mut version = Vec::new();
    for part in rest.split('-') {
        if part.chars().all(|c| c.is_ascii_digit()) {
            // Version numbers, not dates.
            if part.len() <= 2 {
                version.push(part);
            }
        } else {
            let mut chars = part.chars();
            family.push(chars.next().map_or(String::new(), |c| {
                c.to_uppercase().collect::<String>() + chars.as_str()
            }));
        }
    }
    if family.is_empty() {
        return id.to_string();
    }
    let mut name = family.join(" ");
    if !version.is_empty() {
        name = format!("{name} {}", version.join("."));
    }
    if long {
        name.push_str(" (1M)");
    }
    name
}

/// `s` with its first letter in upper case: "claude" → "Claude".
pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or(String::new(), |c| {
        c.to_uppercase().collect::<String>() + chars.as_str()
    })
}

/// `s` on one line, its runs of white space each one space.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `~/…` as a path in the home directory.
pub fn expand_tilde(path: &str) -> String {
    match (path.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
            format!("{}{rest}", home.display())
        }
        _ => path.to_string(),
    }
}

/// `~` instead of the home directory, for shorter paths.
pub fn short_path(path: &std::path::Path) -> String {
    let home = dirs::home_dir();
    match home.as_deref().and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// A project name from a directory: its last part, with anything a name can't
/// hold replaced by '-'.
pub fn directory_name(directory: &std::path::Path) -> String {
    let base = directory
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    base.unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Local wall-clock time of `ms` as HH:MM:SS.
pub fn local_time(ms: i64) -> String {
    let secs = (ms / 1000) as libc::time_t;
    // SAFETY: localtime_r only writes into the tm struct we pass.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
        return "??:??:??".into();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// When `ms` is in local time, to the nearest minute: "19:20" today, else
/// with its day, "Sun 1 Nov 10:34" (as of `now`).
pub fn local_when(ms: i64, now: i64) -> String {
    const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let tm = |ms: i64| {
        let secs = ms.div_euclid(1000) as libc::time_t;
        // SAFETY: localtime_r only writes into the tm struct we pass.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        (!unsafe { libc::localtime_r(&secs, &mut tm) }.is_null()).then_some(tm)
    };
    let (Some(at), Some(today)) = (tm(ms + 30_000), tm(now)) else {
        return "??:??".into();
    };
    let time = format!("{:02}:{:02}", at.tm_hour, at.tm_min);
    if (at.tm_year, at.tm_yday) == (today.tm_year, today.tm_yday) {
        return time;
    }
    format!(
        "{} {} {} {time}",
        DAYS[at.tm_wday.rem_euclid(7) as usize],
        at.tm_mday,
        MONTHS[at.tm_mon.rem_euclid(12) as usize]
    )
}

/// An ISO 8601 time with its offset (`2026-10-25T18:20:00.47+00:00`, or
/// `…Z`), in ms since the epoch.
pub fn iso_ms(text: &str) -> Option<i64> {
    let number = |s: &str| -> Option<i64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let (date, rest) = text.split_once('T')?;
    let mut ymd = date.split('-');
    let (y, m, d) = (
        number(ymd.next()?)?,
        number(ymd.next()?)?,
        number(ymd.next()?)?,
    );
    let zone_at = rest.find(['Z', '+', '-'])?;
    let (time, zone) = rest.split_at(zone_at);
    let (hms, fraction) = time.split_once('.').unwrap_or((time, ""));
    let mut hms = hms.split(':');
    let (h, min, sec) = (
        number(hms.next()?)?,
        number(hms.next()?)?,
        number(hms.next()?)?,
    );
    let ms = match fraction {
        "" => 0,
        f => number(&format!("{f:0<3}")[..3])?,
    };
    let offset = match zone {
        "Z" => 0,
        _ => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let (oh, om) = zone[1..].split_once(':')?;
            sign * (number(oh)? * 60 + number(om)?)
        }
    };
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days since the epoch of a civil date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + h) * 60 + min - offset) * 60_000 + sec * 1000 + ms)
}

/// How long until, briefly: `25m`, `3h 25m`, `6d 18h`.
pub fn until(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    match mins {
        0..60 => format!("{mins}m"),
        60..1440 => format!("{}h {}m", mins / 60, mins % 60),
        _ => format!("{}d {}h", mins / 1440, mins % 1440 / 60),
    }
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
    fn home_is_shortened_by_whole_directories() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(short_path(&home.join("src/app")), "~/src/app");
        assert_eq!(short_path(&home), "~");
        let sibling = format!("{}2/app", home.display());
        assert_eq!(short_path(std::path::Path::new(&sibling)), sibling);
    }

    #[test]
    fn finds_commands_on_the_path() {
        assert!(on_path("sh") && on_path("/bin/sh"));
        assert!(!on_path("no-such-command-here") && !on_path("/no/such/sh"));
    }

    #[test]
    fn quotes_single_quotes() {
        assert_eq!(sh_quote("/a b/roster"), "'/a b/roster'");
        assert_eq!(sh_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn times() {
        assert_eq!(ago(30_000), "now");
        assert_eq!(ago(5 * 60_000), "5m");
        assert_eq!(ago(3 * 3_600_000), "3h");
        assert_eq!(ago(50 * 3_600_000), "2d");
        assert_eq!(clock(12_000), "12s");
        assert_eq!(clock(62 * 60_000), "1h 2m");
        assert_eq!(until(25 * 60_000 + 59_000), "25m");
        assert_eq!(until(205 * 60_000), "3h 25m");
        assert_eq!(until((6 * 24 + 18) * 3_600_000), "6d 18h");
    }

    #[test]
    fn local_times_are_to_the_minute_with_their_day_unless_today() {
        let now = iso_ms("2026-10-25T12:00:00Z").unwrap();
        let at = |t| local_when(iso_ms(t).unwrap(), now);
        // As near the minute as the times services give: a moment before
        // it is that minute.
        assert_eq!(at("2026-10-25T12:19:59.600Z"), at("2026-10-25T12:20:00Z"));
        assert_ne!(at("2026-10-25T12:20:29Z"), at("2026-10-25T12:21:00Z"));
        // (In whatever time zone the tests run, noon UTC is that day there.)
        assert_eq!(at("2026-10-25T12:20:00Z").len(), "12:20".len());
        assert!(at("2026-11-01T12:00:00Z").starts_with("Sun 1 Nov "));
    }

    #[test]
    fn iso_times() {
        assert_eq!(iso_ms("1970-01-01T00:00:00Z"), Some(0));
        // `date -d 2026-10-25T18:20:00Z +%s`
        assert_eq!(
            iso_ms("2026-10-25T18:20:00.471005+00:00"),
            Some(1_792_952_400_471)
        );
        assert_eq!(iso_ms("2026-10-25T20:20:00+02:00"), Some(1_792_952_400_000));
        assert_eq!(iso_ms("2024-02-29T00:00:00.5Z"), Some(1_709_164_800_500));
        assert_eq!(iso_ms("2026-10-25"), None);
        assert_eq!(iso_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(iso_ms("soon"), None);
    }

    #[test]
    fn model_names() {
        assert_eq!(model_name("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(model_name("claude-opus-5-5[1m]"), "Opus 5.5 (1M)");
        assert_eq!(model_name("claude-haiku-5-5"), "Haiku 5.5");
        assert_eq!(model_name("claude-3-5-sonnet-20241022"), "Sonnet 3.5");
        assert_eq!(model_name("opus"), "opus");
        assert_eq!(model_name("claude-5"), "claude-5");
    }

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(b""), "cbf29ce484222325");
        assert_eq!(fnv1a(b"a"), "af63dc4c8601ec8c");
        assert_ne!(fnv1a(b"screen 1"), fnv1a(b"screen 2"));
    }
}
