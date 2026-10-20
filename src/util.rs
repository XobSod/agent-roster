//! Small helpers: clock, terminal size, shell quoting, hashing.

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
    }

    #[test]
    fn fnv1a_is_stable() {
        assert_eq!(fnv1a(b""), "cbf29ce484222325");
        assert_eq!(fnv1a(b"a"), "af63dc4c8601ec8c");
        assert_ne!(fnv1a(b"screen 1"), fnv1a(b"screen 2"));
    }
}
