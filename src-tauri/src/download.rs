//! Streaming download with in-run resume (HTTP Range) and size + sha256 verification.

use std::path::Path;
use std::time::Duration;

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncSeekExt, AsyncWriteExt, BufWriter};
use tokio_util::sync::CancellationToken;
use url::Url;

/// Consecutive failed attempts (without any bytes arriving) before giving up.
const MAX_STALLED_ATTEMPTS: u32 = 8;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("cancelled")]
    Cancelled,
    #[error("the download link has expired — start again from AAB")]
    Expired,
    #[error("the file is no longer in AAB storage (404)")]
    NotFound,
    #[error("download failed: {0}")]
    Http(String),
    #[error("the downloaded file has the wrong size ({got} bytes, expected {expected})")]
    WrongSize { got: u64, expected: u64 },
    #[error("the downloaded file is corrupt (sha256 mismatch)")]
    WrongHash,
    #[error("could not write the file: {0}")]
    Io(#[from] std::io::Error),
}

enum Attempt {
    Done,
    /// Network trouble; retry from the current offset.
    Retry(String),
}

pub async fn download(
    client: &reqwest::Client,
    url: &Url,
    part: &Path,
    size: u64,
    sha256: Option<&str>,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(u64),
) -> Result<(), DownloadError> {
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(part)
        .await?;
    let mut out = BufWriter::with_capacity(4 << 20, file);
    let mut hasher = Sha256::new();
    let mut offset = 0u64;
    let mut stalled = 0u32;

    loop {
        let before = offset;
        let attempt =
            one_attempt(client, url, &mut out, &mut hasher, &mut offset, size, cancel, &mut on_progress)
                .await?;
        match attempt {
            Attempt::Done => break,
            Attempt::Retry(reason) => {
                stalled = if offset > before { 0 } else { stalled + 1 };
                if stalled >= MAX_STALLED_ATTEMPTS {
                    return Err(DownloadError::Http(reason));
                }
                let wait = Duration::from_secs((1u64 << stalled.min(5)).min(30));
                tokio::select! {
                    _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
                    _ = tokio::time::sleep(wait) => {}
                }
            }
        }
    }
    out.flush().await?;
    out.into_inner().sync_all().await?;

    if offset != size {
        return Err(DownloadError::WrongSize { got: offset, expected: size });
    }
    if let Some(expected) = sha256 {
        if hex::encode(hasher.finalize()) != expected {
            return Err(DownloadError::WrongHash);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn one_attempt(
    client: &reqwest::Client,
    url: &Url,
    out: &mut BufWriter<tokio::fs::File>,
    hasher: &mut Sha256,
    offset: &mut u64,
    size: u64,
    cancel: &CancellationToken,
    on_progress: &mut impl FnMut(u64),
) -> Result<Attempt, DownloadError> {
    let mut req = client.get(url.clone());
    if *offset > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={}-", *offset));
    }
    let resp = tokio::select! {
        _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
        r = req.send() => match r {
            Ok(r) => r,
            Err(e) => return Ok(Attempt::Retry(e.without_url().to_string())),
        },
    };

    match resp.status().as_u16() {
        200 if *offset > 0 => {
            // The server ignored the Range header: start over.
            out.flush().await?;
            out.get_mut().set_len(0).await?;
            out.seek(std::io::SeekFrom::Start(0)).await?;
            *hasher = Sha256::new();
            *offset = 0;
        }
        200 => {}
        206 => {
            let want = format!("bytes {}-", *offset);
            let ok = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| v.starts_with(&want));
            if !ok {
                return Err(DownloadError::Http("server resumed at the wrong position".into()));
            }
        }
        416 if *offset == size => return Ok(Attempt::Done),
        403 => return Err(DownloadError::Expired),
        404 => return Err(DownloadError::NotFound),
        s @ (408 | 429 | 500..=599) => return Ok(Attempt::Retry(format!("server answered {s}"))),
        s => return Err(DownloadError::Http(format!("server answered {s}"))),
    }

    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
            n = stream.next() => n,
        };
        match next {
            None => break,
            Some(Err(e)) => return Ok(Attempt::Retry(e.without_url().to_string())),
            Some(Ok(chunk)) => {
                if *offset + chunk.len() as u64 > size {
                    return Err(DownloadError::WrongSize {
                        got: *offset + chunk.len() as u64,
                        expected: size,
                    });
                }
                out.write_all(&chunk).await?;
                hasher.update(&chunk);
                *offset += chunk.len() as u64;
                on_progress(*offset);
            }
        }
    }
    if *offset < size {
        return Ok(Attempt::Retry("connection closed early".into()));
    }
    Ok(Attempt::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[derive(Clone, Copy, Default)]
    struct Mode {
        /// First response closes the connection after this many body bytes.
        drop_first_after: Option<usize>,
        /// Requests numbered >= this (0-based) get 403, like an expired presigned URL.
        expire_from: Option<usize>,
        ignore_range: bool,
        /// Stream slowly so a test can cancel mid-body.
        slow: bool,
    }

    struct Server {
        url: Url,
        ranges: Arc<Mutex<Vec<Option<u64>>>>,
    }

    async fn serve(body: Arc<Vec<u8>>, mode: Mode) -> Server {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/aab-media/v.mov", listener.local_addr().unwrap())).unwrap();
        let ranges = Arc::new(Mutex::new(Vec::new()));
        let count = Arc::new(AtomicUsize::new(0));
        let seen = ranges.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = listener.accept().await.unwrap();
                let (body, seen, count) = (body.clone(), seen.clone(), count.clone());
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                    let start = req
                        .lines()
                        .find_map(|l| l.strip_prefix("range: bytes="))
                        .and_then(|r| r.trim_end_matches('-').trim().parse::<u64>().ok());
                    seen.lock().unwrap().push(start);
                    let i = count.fetch_add(1, Ordering::SeqCst);
                    if mode.expire_from.is_some_and(|e| i >= e) {
                        let _ = sock.write_all(b"HTTP/1.1 403 Forbidden\r\ncontent-length: 0\r\n\r\n").await;
                        return;
                    }
                    let from = if mode.ignore_range { 0 } else { start.unwrap_or(0) as usize };
                    let head = if from > 0 {
                        format!(
                            "HTTP/1.1 206 Partial Content\r\ncontent-length: {}\r\ncontent-range: bytes {}-{}/{}\r\n\r\n",
                            body.len() - from, from, body.len() - 1, body.len()
                        )
                    } else {
                        format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len())
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    let mut slice = &body[from..];
                    if i == 0 {
                        if let Some(d) = mode.drop_first_after {
                            slice = &slice[..d];
                        }
                    }
                    for chunk in slice.chunks(if mode.slow { 1024 } else { 64 * 1024 }) {
                        if sock.write_all(chunk).await.is_err() {
                            return;
                        }
                        if mode.slow {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                    }
                });
            }
        });
        Server { url, ranges }
    }

    fn body() -> Arc<Vec<u8>> {
        Arc::new((0..1_000_000u32).map(|i| (i * 7 % 251) as u8).collect())
    }

    async fn run(server: &Server, body: &[u8], sha: Option<String>, cancel: &CancellationToken)
        -> (Result<(), DownloadError>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("v.mov.part");
        let r = download(&reqwest::Client::new(), &server.url, &part, body.len() as u64,
                         sha.as_deref(), cancel, |_| {}).await;
        if r.is_ok() {
            assert_eq!(std::fs::read(&part).unwrap(), body);
        }
        (r, dir)
    }

    fn sha(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    #[tokio::test]
    async fn downloads_and_verifies() {
        let b = body();
        let s = serve(b.clone(), Mode::default()).await;
        let (r, _d) = run(&s, &b, Some(sha(&b)), &CancellationToken::new()).await;
        r.unwrap();
    }

    #[tokio::test]
    async fn resumes_after_the_connection_drops() {
        let b = body();
        let s = serve(b.clone(), Mode { drop_first_after: Some(300_000), ..Default::default() }).await;
        let (r, _d) = run(&s, &b, Some(sha(&b)), &CancellationToken::new()).await;
        r.unwrap();
        assert_eq!(*s.ranges.lock().unwrap(), vec![None, Some(300_000)]);
    }

    #[tokio::test]
    async fn restarts_when_the_server_ignores_range() {
        let b = body();
        let mode = Mode { drop_first_after: Some(300_000), ignore_range: true, ..Default::default() };
        let s = serve(b.clone(), mode).await;
        let (r, _d) = run(&s, &b, Some(sha(&b)), &CancellationToken::new()).await;
        r.unwrap();
    }

    #[tokio::test]
    async fn reports_expiry_during_resume() {
        let b = body();
        let mode = Mode { drop_first_after: Some(300_000), expire_from: Some(1), ..Default::default() };
        let s = serve(b.clone(), mode).await;
        let (r, _d) = run(&s, &b, None, &CancellationToken::new()).await;
        assert!(matches!(r, Err(DownloadError::Expired)), "{r:?}");
    }

    #[tokio::test]
    async fn rejects_a_wrong_hash() {
        let b = body();
        let s = serve(b.clone(), Mode::default()).await;
        let (r, _d) = run(&s, &b, Some("0".repeat(64)), &CancellationToken::new()).await;
        assert!(matches!(r, Err(DownloadError::WrongHash)), "{r:?}");
    }

    #[tokio::test]
    async fn cancels_mid_body() {
        let b = body();
        let s = serve(b.clone(), Mode { slow: true, ..Default::default() }).await;
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            c.cancel();
        });
        let t = Instant::now();
        let (r, _d) = run(&s, &b, None, &cancel).await;
        assert!(matches!(r, Err(DownloadError::Cancelled)), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    use std::time::Instant;
}
