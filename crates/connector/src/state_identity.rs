//! Conservative candidate-state boundary; identity checks never confer ownership.
use crate::{Result, failure};

pub(crate) fn current() -> Option<String> {
    Some(enumeray_contracts::compatibility::state_identity())
}

pub(crate) fn check(created_with: Option<&str>) -> Result<()> {
    if created_with != Some(enumeray_contracts::compatibility::state_identity().as_str()) {
        return Err(failure(
            "candidate state identity is missing or incompatible; preserve the complete state and original matching binary, inspect retained evidence, and use the release recovery procedure; no automatic migration or recreation is supported",
        ));
    }
    Ok(())
}
