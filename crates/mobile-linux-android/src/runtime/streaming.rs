use mobile_linux_api::MobileLinuxError;
use tokio::io::{AsyncRead, AsyncReadExt, BufReader};

use super::{MAX_CAPTURE_BYTES, MAX_STDOUT_FRAGMENT_BYTES};

pub(super) async fn read_stdout<R, F, Fut>(
    reader: R,
    mut on_line: F,
) -> Result<Vec<u8>, MobileLinuxError>
where
    R: AsyncRead + Unpin,
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), MobileLinuxError>>,
{
    let mut captured = Vec::new();
    let mut pending = Vec::new();
    let mut reader = BufReader::new(reader);
    let mut buffer = vec![0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("read stdout: {error}")))?;
        if read == 0 {
            break;
        }
        append_capped(&mut captured, &buffer[..read]);
        pending.extend_from_slice(&buffer[..read]);
        while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
            let mut line = pending.drain(..=newline).collect::<Vec<_>>();
            trim_line_endings(&mut line);
            on_line(String::from_utf8_lossy(&line).into_owned()).await?;
        }
        if pending.len() >= MAX_STDOUT_FRAGMENT_BYTES {
            let fragment = std::mem::take(&mut pending);
            on_line(String::from_utf8_lossy(&fragment).into_owned()).await?;
        }
    }
    trim_line_endings(&mut pending);
    if !pending.is_empty() {
        on_line(String::from_utf8_lossy(&pending).into_owned()).await?;
    }
    Ok(captured)
}

pub(super) async fn read_stderr<R, F, Fut>(
    mut reader: R,
    mut on_chunk: F,
) -> Result<Vec<u8>, MobileLinuxError>
where
    R: AsyncRead + Unpin,
    F: FnMut(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Result<(), MobileLinuxError>>,
{
    let mut captured = Vec::new();
    let mut buffer = vec![0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|error| MobileLinuxError::Io(format!("read stderr: {error}")))?;
        if read == 0 {
            break;
        }
        let chunk = buffer[..read].to_vec();
        append_capped(&mut captured, &chunk);
        on_chunk(chunk).await?;
    }
    Ok(captured)
}

pub(super) async fn join_reader(
    task: tokio::task::JoinHandle<Result<Vec<u8>, MobileLinuxError>>,
    stream: &str,
) -> Result<Vec<u8>, MobileLinuxError> {
    task.await
        .map_err(|error| MobileLinuxError::Io(format!("{stream} reader failed: {error}")))?
}

pub(super) fn append_capped(captured: &mut Vec<u8>, chunk: &[u8]) {
    let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
    if remaining == 0 {
        return;
    }
    captured.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
}

pub(super) fn trim_line_endings(line: &mut Vec<u8>) {
    while matches!(line.last(), Some(b'\n' | b'\r')) {
        line.pop();
    }
}
