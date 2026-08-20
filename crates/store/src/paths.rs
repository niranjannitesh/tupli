//! Where the app keeps its things.
//!
//! Apple's layout, not XDG's: a native app that scatters dotfiles in `$HOME` is
//! a port, and this one isn't.

use std::path::PathBuf;

/// `~/Library/Application Support/tupli` — the database, and anything else that
/// is state rather than preference.
pub fn data_dir() -> PathBuf {
    home().join("Library/Application Support/tupli")
}

/// `~/Library/Logs/tupli` — where Console.app already knows to look.
pub fn log_dir() -> PathBuf {
    home().join("Library/Logs/tupli")
}

/// The SQLite file. One file: connections, history, saved queries and window
/// state all live together because they are all "this machine's memory of the
/// app", and a single file is a single thing to back up.
pub fn database_file() -> PathBuf {
    data_dir().join("tupli.db")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        // A process with no HOME is a launchd job or a broken shell. Falling
        // back to the working directory keeps it running rather than panicking
        // three frames into boot.
        .unwrap_or_else(|| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_lands_under_library() {
        assert!(data_dir().ends_with("Library/Application Support/tupli"));
        assert!(database_file().ends_with("tupli.db"));
        assert!(log_dir().ends_with("Library/Logs/tupli"));
    }
}
