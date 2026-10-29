//! Host consent is invocation-local, never recovered from installation evidence.
use crate::{Result, failure};
use enumeray_contracts::execution::ExecutionMode;

pub(crate) const WARNING: &str = "Development execution reduces isolation: trusted model code runs as your OS user and can access your connector credentials and files.";

pub(crate) fn warn(allowed: bool) {
    if allowed {
        eprintln!(
            "{}",
            serde_json::json!({"status":"development_execution_warning", "message":WARNING})
        );
    }
}

pub(crate) fn require_consent(mode: ExecutionMode, allowed: bool) -> Result<()> {
    if mode == ExecutionMode::Development && !allowed {
        return Err(failure(
            "development execution is blocked; restart with --allow-development-execution only for trusted models; accepted work remains recoverable",
        ));
    }
    Ok(())
}

pub(crate) fn check_identity(mode: ExecutionMode, connector: &str, runner: &str) -> Result<()> {
    fn uid(value: &str) -> Result<u32> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(failure("execution UID evidence is missing or invalid"));
        }
        value
            .parse()
            .map_err(|_| failure("execution UID evidence is invalid"))
    }
    let connector = uid(connector)?;
    let runner = uid(runner)?;
    match mode {
        ExecutionMode::Managed if connector == runner => Err(failure(
            "managed direct launcher must use a separate execution identity",
        )),
        ExecutionMode::Development if connector == 0 || runner != connector => Err(failure(
            "development execution requires the connector and runner to use the same non-root OS user",
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consent_never_relaxes_managed_identity_or_accepts_root_or_missing_evidence() {
        use ExecutionMode::{Development, Managed};
        assert!(require_consent(Development, false).is_err());
        assert!(require_consent(Development, true).is_ok());
        assert!(require_consent(Managed, false).is_ok());
        assert!(check_identity(Managed, "501", "501").is_err());
        assert!(check_identity(Managed, "501", "502").is_ok());
        assert!(check_identity(Development, "501", "501").is_ok());
        for (connector, runner) in [
            ("0", "0"),
            ("0", "501"),
            ("501", "502"),
            ("501", ""),
            ("", "501"),
            ("unknown", "501"),
            ("501", "-1"),
            ("501", "4294967296"),
        ] {
            assert!(check_identity(Development, connector, runner).is_err());
        }
        assert!(check_identity(Managed, "unknown", "502").is_err());
        assert!(check_identity(Managed, "501", "unknown").is_err());
    }
}
