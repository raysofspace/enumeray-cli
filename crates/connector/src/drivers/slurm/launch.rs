//! Freeze launch identity and installation evidence without interpreting Slurm directives.
use super::*;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, time::SystemTime};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LaunchIntent {
    pub intent: Uuid,
    pub script_digest: String,
    pub execution_uid: String,
    pub prepared_at: i64,
}

pub(crate) fn read_script(path: &Path) -> Result<(String, String)> {
    let file = File::open(path).map_err(|_| failure("cannot read Slurm batch file"))?;
    let metadata = file
        .metadata()
        .map_err(|_| failure("cannot inspect Slurm batch file"))?;
    if !metadata.is_file() || metadata.len() > local_process::MAX_RECORD_BYTES {
        return Err(failure(
            "Slurm batch file must be a regular file of at most 4 MiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take(local_process::MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| failure("cannot read Slurm batch file"))?;
    if bytes.len() as u64 > local_process::MAX_RECORD_BYTES
        || !bytes.starts_with(b"#!")
        || bytes.contains(&0)
        || bytes.contains(&b'\r')
    {
        return Err(failure("invalid Slurm batch file"));
    }
    let digest = STANDARD.encode(Sha256::digest(&bytes));
    let script = String::from_utf8(bytes).map_err(|_| failure("Slurm batch file must be UTF-8"))?;
    Ok((script, digest))
}

pub(super) fn prepare(job: &ComputeJob) -> Result<LaunchIntent> {
    if !matches!(job.execution.driver, DriverConfig::Slurm(_)) {
        return Err(failure("Slurm driver required"));
    }
    let script = job
        .execution
        .slurm_script
        .as_deref()
        .ok_or_else(|| failure("Slurm batch file required"))?;
    let (_, digest) = read_script(Path::new(script))?;
    if job.execution.installation.file_digests.get(script) != Some(&digest) {
        return Err(failure(
            "installed Slurm batch file changed after validation",
        ));
    }
    Ok(LaunchIntent {
        intent: job.intent,
        script_digest: digest,
        execution_uid: crate::compute::execution_uid()?,
        prepared_at: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|_| failure("invalid launch time"))?
            .as_secs()
            .try_into()
            .map_err(|_| failure("invalid launch time"))?,
    })
}

pub(super) fn load(directory: &Path, job: &ComputeJob) -> Result<LaunchIntent> {
    let record: LaunchIntent = local_process::read_record(&directory.join("launch-intent.json"))?;
    if !matches!(job.execution.driver, DriverConfig::Slurm(_)) {
        return Err(failure("Slurm driver required"));
    }
    if record.intent != job.intent
        || record.intent.is_nil()
        || record.execution_uid != crate::compute::execution_uid()?
        || record.prepared_at <= 0
        || job
            .execution
            .slurm_script
            .as_ref()
            .and_then(|p| job.execution.installation.file_digests.get(p))
            != Some(&record.script_digest)
    {
        return Err(failure("invalid persisted Slurm launch intent"));
    }
    Ok(record)
}

pub(super) fn accounting_start(prepared_at: i64) -> Result<String> {
    // Allow bounded submit-host/accounting clock skew; never lose old jobs to a
    // moving 'now-7days' window. Commands use UTC for this absolute timestamp.
    let date = time::OffsetDateTime::from_unix_timestamp(prepared_at.saturating_sub(300))
        .map_err(|_| failure("invalid persisted Slurm launch time"))?;
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        date.year(),
        u8::from(date.month()),
        date.day(),
        date.hour(),
        date.minute(),
        date.second()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounting_window_is_absolute_and_survives_long_outages() {
        assert_eq!(
            accounting_start(1_700_000_000).unwrap(),
            "2023-11-14T22:08:20"
        );
    }
}
