use std::{
    cmp::Ordering,
    io::{self, Write},
    sync::{LazyLock, OnceLock},
};

use enumeray_contracts::compatibility::CLI_VERSION;
use semver::Version;

static INSTALLED: LazyLock<Version> =
    LazyLock::new(|| Version::parse(CLI_VERSION).expect("Cargo package version"));
static NOTICE: OnceLock<(Version, bool)> = OnceLock::new();

// Keep the first mismatch until the command knows whether stderr is reserved
// for a structured runtime error.
pub(crate) fn record(recommended: &str) {
    if recommended.len() > 100 || NOTICE.get().is_some() {
        return;
    }
    let Ok(recommended) = Version::parse(recommended) else {
        return;
    };
    let precedence = INSTALLED.cmp_precedence(&recommended);
    if precedence != Ordering::Equal {
        let _ = NOTICE.set((recommended, precedence == Ordering::Less));
    }
}

pub(crate) fn write_notice() {
    if let Some((recommended, upgrade)) = NOTICE.get() {
        let upgrade = if *upgrade {
            " Upgrade at https://github.com/raysofspace/enumeray-cli/releases."
        } else {
            ""
        };
        let _ = writeln!(
            io::stderr().lock(),
            "enumeray: This service recommends CLI {recommended}. You are using {CLI_VERSION}.{upgrade}"
        );
    }
}
