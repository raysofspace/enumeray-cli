//! Shared bulk-transfer bounds, independent of API/control request deadlines.
use std::time::Duration;

/// Transport selection applies only when creating a new reservation. Existing
/// reservations retain their originally accepted transport and lifetime.
pub const MULTIPART_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;
pub fn uses_multipart(size: u64) -> bool {
    size > MULTIPART_THRESHOLD_BYTES
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransferBudgets {
    pub connect: Duration,
    pub stall: Duration,
    pub overall: Duration,
}
impl Default for TransferBudgets {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(15),
            stall: Duration::from_secs(60),
            overall: Duration::from_secs(172800),
        }
    }
}
impl TransferBudgets {
    /// Sites may shorten these budgets. The maximum overall bound also governs
    /// abandoned-byte accounting, so it cannot be extended by a client setting.
    pub fn from_env() -> Result<Self, &'static str> {
        Self::parse(|name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err("bulk-transfer budget is not valid text"),
        })
    }
    fn parse(
        mut get: impl FnMut(&str) -> Result<Option<String>, &'static str>,
    ) -> Result<Self, &'static str> {
        let mut seconds = |name, maximum| match get(name)? {
            None => Ok(Duration::from_secs(maximum)),
            Some(value) => value
                .parse::<u64>()
                .ok()
                .filter(|n| (1..=maximum).contains(n))
                .map(Duration::from_secs)
                .ok_or("bulk-transfer budget exceeds its allowed range"),
        };
        Ok(Self {
            connect: seconds("ENUMERAY_TRANSFER_CONNECT_SECONDS", 15)?,
            stall: seconds("ENUMERAY_TRANSFER_STALL_SECONDS", 60)?,
            overall: seconds("ENUMERAY_TRANSFER_OVERALL_SECONDS", 172800)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multipart_selection_is_independent_of_the_single_put_ceiling() {
        assert!(!uses_multipart(0));
        assert!(!uses_multipart(MULTIPART_THRESHOLD_BYTES));
        assert!(uses_multipart(MULTIPART_THRESHOLD_BYTES + 1));
        assert!(uses_multipart(crate::models::MAX_SINGLE_UPLOAD_BYTES));
        assert!(uses_multipart(crate::models::MAX_SINGLE_UPLOAD_BYTES + 1));
    }
    #[test]
    fn bulk_settings_preserve_hard_cleanup_bounds() {
        assert_eq!(
            TransferBudgets::parse(|_| Ok(None)).unwrap(),
            TransferBudgets::default()
        );
        let short = TransferBudgets::parse(|_| Ok(Some("1".into()))).unwrap();
        assert_eq!(short.overall, Duration::from_secs(1));
        for value in ["0", "-1", "172801", "18446744073709551616", "x"] {
            assert!(TransferBudgets::parse(|_| Ok(Some(value.into()))).is_err());
        }
    }
}
