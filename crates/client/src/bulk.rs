//! Credential-free storage I/O with independent connection, stall and overall
//! budgets. Fixed chunks keep memory independent of an artifact's size.
use crate::{Error, validate_url};
use enumeray_contracts::transfers::TransferBudgets;
use futures_util::{StreamExt, stream};
use std::{
    fs::File,
    io::Write,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::AsyncReadExt;
mod checkpoint;
pub use checkpoint::{
    CHECKPOINT_BYTES, DownloadEvidence, RANGE_BYTES, RetainedTransfer, TransferProgress,
    checkpoint_directory,
};
const CHUNK: usize = 64 * 1024;
fn runtime() -> Result<tokio::runtime::Runtime, Error> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(Error::Io)
}
fn client(overall: Duration, budgets: TransferBudgets) -> Result<reqwest::Client, Error> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(budgets.connect)
        .read_timeout(budgets.stall)
        .timeout(overall.min(budgets.overall))
        .build()?)
}

/// Download exact, version-pinned ranges into a locked retained payload. The
/// authorization callback is called before every request, including one refresh
/// after storage rejects an expired capability. Its URL is never persisted.
pub fn download_retained(
    retained: &mut RetainedTransfer,
    mut authorize: impl FnMut() -> Result<String, Error>,
    mut progress: impl FnMut(TransferProgress),
    budgets: TransferBudgets,
    deadline: Instant,
) -> Result<(), Error> {
    if retained.is_published() {
        progress(retained.progress());
        return Ok(());
    }
    let version =
        retained.evidence().version_id.clone().ok_or_else(|| {
            Error::Invalid("HTTP download requires an exact object version.".into())
        })?;
    retained.discard_uncommitted()?;
    progress(retained.progress());
    let runtime = runtime()?;
    let http = runtime
        .block_on(async { client(deadline.saturating_duration_since(Instant::now()), budgets) })?;
    let mut empty = retained.evidence().size_bytes == 0;
    while retained.offset() < retained.evidence().size_bytes || empty {
        retained.check_deadline()?;
        let start = retained.offset();
        let length = (retained.evidence().size_bytes - start).min(RANGE_BYTES);
        let mut refreshed = false;
        loop {
            retained.check_deadline()?;
            let url = validate_url(&authorize()?)?;
            let mut request = http
                .get(url)
                .header("accept-encoding", "identity")
                .timeout(deadline.saturating_duration_since(Instant::now()));
            if length > 0 {
                request = request.header("range", format!("bytes={start}-{}", start + length - 1));
            }
            let response = runtime.block_on(async { request.send().await })?;
            if matches!(response.status().as_u16(), 401 | 403) && !refreshed {
                refreshed = true;
                continue;
            }
            if response.status().as_u16() != if empty { 200 } else { 206 } {
                return Err(Error::Invalid(format!(
                    "Storage range download failed (HTTP {}); retain progress and repeat the same command.",
                    response.status().as_u16()
                )));
            }
            let headers = response.headers();
            let expected_range = format!(
                "bytes {start}-{}/{}",
                start + length.saturating_sub(1),
                retained.evidence().size_bytes
            );
            if headers
                .get("x-amz-version-id")
                .and_then(|h| h.to_str().ok())
                != Some(version.as_str())
                || headers
                    .get("content-length")
                    .and_then(|h| h.to_str().ok())
                    .and_then(|s| s.parse::<u64>().ok())
                    != Some(length)
                || !empty
                    && headers.get("content-range").and_then(|h| h.to_str().ok())
                        != Some(expected_range.as_str())
                || headers
                    .get("content-encoding")
                    .is_some_and(|h| h != "identity")
            {
                return Err(Error::Invalid(
                    "Storage range, length or version conflicts with the pinned download.".into(),
                ));
            }
            let result = runtime.block_on(async {
                let mut body = response.bytes_stream();
                let mut received = 0;
                while let Some(chunk) = tokio::time::timeout(budgets.stall, body.next())
                    .await
                    .map_err(|_| {
                        Error::Invalid(
                            "Storage download stalled; repeat the same command to resume.".into(),
                        )
                    })?
                {
                    let chunk = chunk?;
                    received += chunk.len() as u64;
                    if received > length {
                        return Err(Error::Invalid(
                            "Storage returned an oversized range.".into(),
                        ));
                    }
                    retained.append(&chunk)?;
                    progress(retained.progress());
                }
                if received != length {
                    return Err(Error::Invalid(
                        "Storage range was truncated; repeat the same command to resume.".into(),
                    ));
                }
                Ok(())
            });
            // Save bytes from an interrupted, identity-checked response as well.
            retained.checkpoint()?;
            result?;
            empty = false;
            break;
        }
    }
    Ok(())
}
/// Send only the provided storage capability; never an API bearer or cookie.
pub fn upload(
    mut file: File,
    offset: u64,
    size: u64,
    url: &str,
    headers: &[(String, String)],
    overall: Duration,
    progress: impl Fn(u64) + Send + Sync + 'static,
) -> Result<Option<String>, Error> {
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(offset))?;
    let url = validate_url(url)?;
    for (name, _) in headers {
        if ["authorization", "host", "cookie"]
            .iter()
            .any(|blocked| name.eq_ignore_ascii_case(blocked))
        {
            return Err(Error::Invalid(
                "Unexpected storage credential header.".into(),
            ));
        }
    }
    let budgets = TransferBudgets::from_env().map_err(|message| Error::Invalid(message.into()))?;
    let overall = overall.min(budgets.overall);
    if overall.is_zero() {
        return Err(Error::Invalid(
            "Storage upload reached its overall deadline; resume the same operation.".into(),
        ));
    }
    runtime()?.block_on(async {
        let bytes=Arc::new(AtomicU64::new(0));let count=Arc::clone(&bytes);let progress=Arc::new(progress);
        let file=tokio::fs::File::from_std(file).take(size);
        let body=stream::try_unfold((file,size,count,progress),|(mut file,remaining,count,progress)|async move {
            if remaining==0 {return Ok::<_,std::io::Error>(None);}
            let mut chunk=vec![0;remaining.min(CHUNK as u64) as usize];let n=file.read(&mut chunk).await?;
            if n==0 {return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof,"upload source changed"));}
            chunk.truncate(n);count.fetch_add(n as u64,Ordering::Relaxed);progress(n as u64);
            Ok(Some((chunk,(file,remaining-n as u64,count,progress))))
        });
        let mut request=client(overall, budgets)?.put(url).header(reqwest::header::CONTENT_LENGTH,size);
        for (name,value) in headers {request=request.header(name,value);}
        let sent=request.body(reqwest::Body::wrap_stream(body)).send();tokio::pin!(sent);
        let mut last_bytes=0;let mut last_progress=Instant::now();let started=Instant::now();
        loop {tokio::select! {
            result=&mut sent=>{let response=result?;if !response.status().is_success(){return Err(Error::Invalid(format!("Storage upload failed (HTTP {}). Refresh the same transfer authorization.",response.status().as_u16())));}
if bytes.load(Ordering::Relaxed)!=size{return Err(Error::Invalid("Storage did not consume the expected upload bytes.".into()));}return Ok(response.headers().get("x-amz-version-id").and_then(|v|v.to_str().ok()).filter(|v|!v.is_empty()&&*v!="null").map(str::to_owned));},
            ()=tokio::time::sleep(Duration::from_secs(1))=>{let current=bytes.load(Ordering::Relaxed);if current!=last_bytes {last_bytes=current;last_progress=Instant::now();}
if started.elapsed()>=overall||last_progress.elapsed()>=budgets.stall{return Err(Error::Invalid("Storage upload stalled or reached its bulk deadline; resume the same operation.".into()));}}
        }}
    })
}
pub fn download(url: &str, output: &mut impl Write, progress: impl Fn(u64)) -> Result<u64, Error> {
    let url = validate_url(url)?;
    let budgets = TransferBudgets::from_env().map_err(|message| Error::Invalid(message.into()))?;
    runtime()?.block_on(async {
        let response = client(budgets.overall, budgets)?.get(url).send().await?;
        if !response.status().is_success() {
            return Err(Error::Invalid(format!(
                "Storage download failed (HTTP {}).",
                response.status().as_u16()
            )));
        }
        let mut body = response.bytes_stream();
        let mut count = 0_u64;
        while let Some(chunk) = tokio::time::timeout(budgets.stall, body.next())
            .await
            .map_err(|_| {
                Error::Invalid(
                    "Storage download stalled; request fresh authorization and retry.".into(),
                )
            })?
        {
            let chunk = chunk?;
            output.write_all(&chunk)?;
            count = count
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| Error::Invalid("Download byte count overflow.".into()))?;
            progress(chunk.len() as u64);
        }
        Ok(count)
    })
}
