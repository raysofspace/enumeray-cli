//! A fixed SFTP v3 challenge on one verified, already-open sentinel handle.
//! Never create/truncate a remote path or write bytes obtained from the server.
use super::{ComputeTarget, Result, connections, failure, probe_name, sentinel};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

const MAX_PACKET: usize = 8192;

pub(super) fn probe(target: &ComputeTarget, id: Uuid, remaining: Duration) -> Result<()> {
    let command = connections::sftp_target_command(target)?;
    let path = Path::new(&target.work_root)
        .join(probe_name(id))
        .join("sentinel");
    let path = path
        .to_str()
        .ok_or_else(|| failure("invalid SFTP probe path"))?;
    let expected = sentinel(id);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| failure("cannot supervise SFTP probe"))?;
    runtime.block_on(async {
        let mut child = tokio::process::Command::from(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| failure("SFTP transport unavailable"))?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| failure("SFTP input unavailable"))?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| failure("SFTP output unavailable"))?;
        tokio::time::timeout(remaining.min(Duration::from_secs(30)), async move {
            roundtrip(&mut input, &mut output, path, &expected).await?;
            drop(input);
            drop(output);
            let status = child
                .wait()
                .await
                .map_err(|_| failure("SFTP completion unavailable"))?;
            if !status.success() {
                return Err(failure("SFTP transport did not complete"));
            }
            Ok(())
        })
        .await
        .map_err(|_| failure("SFTP probe timed out"))?
    })
}

fn string(value: &[u8], into: &mut Vec<u8>) {
    into.extend_from_slice(&(value.len() as u32).to_be_bytes());
    into.extend_from_slice(value);
}
fn parse_string(bytes: &[u8], max: usize) -> Result<&[u8]> {
    let length = bytes
        .get(..4)
        .and_then(|s| s.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| failure("invalid SFTP string"))? as usize;
    if length > max || bytes.len() != length + 4 {
        return Err(failure("SFTP string exceeds bound"));
    }
    Ok(&bytes[4..])
}
async fn send(out: &mut (impl AsyncWrite + Unpin), kind: u8, body: &[u8]) -> Result<()> {
    if body.len() >= MAX_PACKET {
        return Err(failure("SFTP request exceeds bound"));
    }
    out.write_all(&((body.len() + 1) as u32).to_be_bytes())
        .await
        .map_err(|_| failure("SFTP write failed"))?;
    out.write_all(&[kind])
        .await
        .map_err(|_| failure("SFTP write failed"))?;
    out.write_all(body)
        .await
        .map_err(|_| failure("SFTP write failed"))?;
    out.flush().await.map_err(|_| failure("SFTP write failed"))
}
async fn receive(
    input: &mut (impl AsyncRead + Unpin),
    kind: u8,
    id: Option<u32>,
) -> Result<Vec<u8>> {
    let size = input
        .read_u32()
        .await
        .map_err(|_| failure("SFTP response unavailable"))? as usize;
    if !(1..=MAX_PACKET).contains(&size) {
        return Err(failure("SFTP response exceeds bound"));
    }
    let mut bytes = vec![0; size];
    input
        .read_exact(&mut bytes)
        .await
        .map_err(|_| failure("SFTP response incomplete"))?;
    if bytes[0] != kind {
        return Err(failure("SFTP request rejected or protocol incompatible"));
    }
    let offset = if let Some(id) = id {
        if bytes.get(1..5) != Some(id.to_be_bytes().as_slice()) {
            return Err(failure("SFTP response identity mismatch"));
        }
        5
    } else {
        1
    };
    Ok(bytes[offset..].to_vec())
}
fn handle_request(id: u32, handle: &[u8]) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    string(handle, &mut body);
    body
}
async fn status(input: &mut (impl AsyncRead + Unpin), id: u32) -> Result<()> {
    let body = receive(input, 101, Some(id)).await?;
    if body.get(..4) != Some(0u32.to_be_bytes().as_slice()) {
        return Err(failure("SFTP operation rejected"));
    }
    Ok(())
}
async fn read_verified(
    input: &mut (impl AsyncRead + Unpin),
    out: &mut (impl AsyncWrite + Unpin),
    id: u32,
    handle: &[u8],
    expected: &[u8],
) -> Result<()> {
    let mut body = handle_request(id, handle);
    body.extend_from_slice(&0u64.to_be_bytes());
    body.extend_from_slice(&((expected.len() + 1) as u32).to_be_bytes());
    send(out, 5, &body).await?;
    let data = receive(input, 103, Some(id)).await?;
    if parse_string(&data, expected.len() + 1)? != expected {
        return Err(failure(
            "SFTP probe identity changed; no foreign data is returned",
        ));
    }
    Ok(())
}
async fn roundtrip(
    out: &mut (impl AsyncWrite + Unpin),
    input: &mut (impl AsyncRead + Unpin),
    path: &str,
    expected: &[u8],
) -> Result<()> {
    send(out, 1, &3u32.to_be_bytes()).await?;
    let version = receive(input, 2, None).await?;
    if version.get(..4) != Some(3u32.to_be_bytes().as_slice()) {
        return Err(failure("SFTP v3 unavailable"));
    }
    let mut body = 1u32.to_be_bytes().to_vec();
    string(path.as_bytes(), &mut body);
    // READ|WRITE only. In particular, CREAT, TRUNC and APPEND are absent.
    body.extend_from_slice(&3u32.to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes());
    send(out, 3, &body).await?;
    let opened = receive(input, 102, Some(1)).await?;
    let handle = parse_string(&opened, 256)?;
    send(out, 8, &handle_request(2, handle)).await?;
    let attributes = receive(input, 105, Some(2)).await?;
    let mut attrs = attributes.as_slice();
    let flags = attrs
        .read_u32()
        .await
        .map_err(|_| failure("SFTP attributes unavailable"))?;
    if flags & 5 != 5 {
        return Err(failure("SFTP file type or size unverified"));
    }
    let size = attrs
        .read_u64()
        .await
        .map_err(|_| failure("SFTP size unavailable"))?;
    if flags & 2 != 0 {
        attrs
            .read_u64()
            .await
            .map_err(|_| failure("SFTP identity unavailable"))?;
    }
    let mode = attrs
        .read_u32()
        .await
        .map_err(|_| failure("SFTP file type unavailable"))?;
    if size != expected.len() as u64 || mode & 0o170000 != 0o100000 {
        return Err(failure(
            "SFTP probe is not the expected private regular file",
        ));
    }
    read_verified(input, out, 3, handle, expected).await?;
    let mut write = handle_request(4, handle);
    write.extend_from_slice(&0u64.to_be_bytes());
    string(expected, &mut write);
    send(out, 6, &write).await?;
    status(input, 4).await?;
    read_verified(input, out, 5, handle, expected).await?;
    send(out, 4, &handle_request(6, handle)).await?;
    status(input, 6).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn sftp_challenge_uses_one_handle_and_never_creates_or_truncates_paths() {
        for substituted in [false, true] {
            let expected = sentinel(Uuid::new_v4());
            let challenge = expected.clone();
            let (client, server) = tokio::io::duplex(4096);
            let (mut input, mut out) = tokio::io::split(client);
            let task = tokio::spawn(async move {
                let (mut input, mut out) = tokio::io::split(server);
                assert_eq!(
                    receive(&mut input, 1, None).await.unwrap(),
                    3u32.to_be_bytes()
                );
                send(&mut out, 2, &3u32.to_be_bytes()).await.unwrap();
                let open = receive(&mut input, 3, Some(1)).await.unwrap();
                assert_eq!(&open[open.len() - 8..], &[0, 0, 0, 3, 0, 0, 0, 0]);
                let mut handle = 1u32.to_be_bytes().to_vec();
                string(b"handle", &mut handle);
                send(&mut out, 102, &handle).await.unwrap();
                assert_eq!(
                    receive(&mut input, 8, Some(2)).await.unwrap(),
                    handle_request(2, b"handle")[4..]
                );
                let mut attrs = 2u32.to_be_bytes().to_vec();
                attrs.extend_from_slice(&5u32.to_be_bytes());
                attrs.extend_from_slice(&(challenge.len() as u64).to_be_bytes());
                attrs.extend_from_slice(&0o100600u32.to_be_bytes());
                send(&mut out, 105, &attrs).await.unwrap();
                receive(&mut input, 5, Some(3)).await.unwrap();
                let mut data = 3u32.to_be_bytes().to_vec();
                string(
                    if substituted {
                        b"unrelated private data"
                    } else {
                        &challenge
                    },
                    &mut data,
                );
                send(&mut out, 103, &data).await.unwrap();
                if substituted {
                    // Verification failure must close the channel before any WRITE.
                    assert_eq!(input.read(&mut [0]).await.unwrap(), 0);
                    return;
                }
                let write = receive(&mut input, 6, Some(4)).await.unwrap();
                let mut approved = handle_request(4, b"handle")[4..].to_vec();
                approved.extend_from_slice(&0u64.to_be_bytes());
                string(&challenge, &mut approved);
                assert_eq!(write, approved);
                let mut ok = 4u32.to_be_bytes().to_vec();
                ok.extend_from_slice(&0u32.to_be_bytes());
                send(&mut out, 101, &ok).await.unwrap();
                receive(&mut input, 5, Some(5)).await.unwrap();
                let mut data = 5u32.to_be_bytes().to_vec();
                string(&challenge, &mut data);
                send(&mut out, 103, &data).await.unwrap();
                receive(&mut input, 4, Some(6)).await.unwrap();
                ok[..4].copy_from_slice(&6u32.to_be_bytes());
                send(&mut out, 101, &ok).await.unwrap();
            });
            let result =
                roundtrip(&mut out, &mut input, "/approved/probe/sentinel", &expected).await;
            assert_eq!(result.is_err(), substituted);
            drop(input);
            drop(out);
            task.await.unwrap();
        }
    }
    #[tokio::test]
    async fn oversized_and_mismatched_sftp_frames_are_rejected() {
        let size = ((MAX_PACKET + 1) as u32).to_be_bytes();
        assert!(receive(&mut size.as_slice(), 2, None).await.is_err());
        let mut wire = Vec::new();
        wire.extend_from_slice(&5u32.to_be_bytes());
        wire.push(101);
        wire.extend_from_slice(&99u32.to_be_bytes());
        assert!(receive(&mut wire.as_slice(), 101, Some(1)).await.is_err());
    }
}
