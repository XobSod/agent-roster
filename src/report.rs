//! `roster report <kind>`: called by agent hooks. Inserts one event and exits.
//!
//! It must be silent and always exit 0, whatever goes wrong: for Claude, a `Stop`
//! hook exiting with code 2 means "don't stop, keep going". That's also why `main`
//! dispatches here before clap parses (and possibly rejects) the arguments.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};

use crate::paths::Paths;
use crate::util::now_ms;

/// Hook payloads are small JSON objects; anything bigger is cut off.
const MAX_PAYLOAD: u64 = 1 << 20;

pub fn run(args: &[String]) -> ! {
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(|| report(args));
    std::process::exit(0);
}

fn report(args: &[String]) -> Result<()> {
    let kind = args.first().context("no event kind")?;
    let session = std::env::var("ROSTER_SESSION").context("ROSTER_SESSION not set")?;
    let mut payload = String::new();
    // Hooks get JSON on stdin; when run by hand from a terminal, don't wait for it.
    if !std::io::stdin().is_terminal() {
        std::io::stdin()
            .take(MAX_PAYLOAD)
            .read_to_string(&mut payload)?;
    }
    let payload = payload.trim();
    let db = match std::env::var_os("ROSTER_DB") {
        Some(path) => PathBuf::from(path),
        None => Paths::from_env()?.db(),
    };
    // Never create the database or run migrations from a hook.
    let conn = Connection::open_with_flags(
        db,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_millis(500))?;
    let payload = (!payload.is_empty()).then_some(payload);
    crate::db::insert_event(&conn, &session, kind, now_ms(), payload)
}
