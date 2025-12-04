#![warn(missing_docs)]
#![doc = include_str!("../README.md")]

use futures_util::{FutureExt, StreamExt, future::BoxFuture, stream::BoxStream};
use std::{num::NonZeroU64, task::ready};
use tokio::io::{AsyncRead, AsyncSeek};

type RequestFuture = BoxFuture<'static, reqwest::Result<ResponseStream>>;
type ResponseStream = BoxStream<'static, reqwest::Result<bytes::Bytes>>;

fn new_request(client: &reqwest::Client, url: reqwest::Url, pos: u64) -> RequestFuture {
    client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={}-", pos))
        .send()
        .map(|resp| match resp {
            Ok(resp) => match resp.error_for_status() {
                Ok(resp) => Ok(resp.bytes_stream().boxed()),
                Err(e) => Err(e),
            },
            Err(e) => Err(e),
        })
        .boxed()
}

/// An remote file accessed over HTTP.
/// Implements `AsyncRead` and `AsyncSeek` traits.
///
/// * Supports seeking and reading at arbitrary positions.
/// * Uses HTTP Range requests to fetch data.
/// * Handles transient network errors with retries.
/// * `stream_position()` is cheap, as it is tracked locally.
///
pub struct HttpFile {
    client: reqwest::Client,

    // info
    url: reqwest::Url,
    content_length: Option<NonZeroU64>,
    etag: Option<String>,
    mime: Option<String>,

    // inner states
    pos: u64,
    request: Option<(u64, RequestFuture)>,
    response: Option<ResponseStream>,
    last_chunk: Option<bytes::Bytes>,
    seek: Option<u64>,
    retry_attempt: u8,
}

impl std::fmt::Debug for HttpFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpFile")
            .field("client", &self.client)
            .field("url", &self.url)
            .field("content_length", &self.content_length)
            .field("etag", &self.etag)
            .field("pos", &self.pos)
            .field(
                "request",
                &self
                    .request
                    .as_ref()
                    .map(|(pos, _)| format!("request at {}", pos)),
            )
            .field("response", &"[response stream]")
            .field("last_chunk", &self.last_chunk)
            .field("seek", &self.seek)
            .finish()
    }
}

impl HttpFile {
    /// url of the file
    pub fn url(&self) -> &reqwest::Url {
        &self.url
    }
    /// content length of the file(in bytes), if present
    pub fn content_length(&self) -> Option<u64> {
        self.content_length.map(|v| v.get())
    }
    /// etag of the file, if present
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }
    /// Mime type of the file, if present
    pub fn mime(&self) -> Option<&str> {
        self.mime.as_deref()
    }
}

impl HttpFile {
    /// Create a new `HttpFile` from a `reqwest::Client` and a file URL.
    ///
    /// Arguments:
    /// * `client`: A `reqwest::Client` instance to make HTTP requests.
    /// * `url`: The URL of the file to access.
    ///
    pub async fn new(client: reqwest::Client, url: &str) -> reqwest::Result<Self> {
        log::debug!("HEAD {}", url);
        let resp = client.head(url).send().await?.error_for_status()?;
        let etag = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let content_length = resp
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<NonZeroU64>().ok());

        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let url = resp.url().clone();
        let pos = 0;

        Ok(Self {
            client,
            content_length,
            url,
            pos,
            request: None,
            response: None,
            last_chunk: None,
            seek: None,
            etag,
            retry_attempt: 3,
            mime,
        })
    }

    fn reset_retry(&mut self) {
        self.retry_attempt = 3;
    }
}

impl AsyncRead for HttpFile {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        // Check if we're at or beyond the end of file
        if let Some(content_length) = self.content_length {
            if self.pos >= content_length.get() {
                return std::task::Poll::Ready(Ok(()));
            }
        }

        if let Some(last_chunk) = self.last_chunk.take() {
            let size = last_chunk.len().min(buf.remaining());
            buf.put_slice(&last_chunk[..size]);
            self.pos += size as u64;
            if size < last_chunk.len() {
                self.last_chunk = Some(last_chunk.slice(size..));
            }
            return std::task::Poll::Ready(Ok(()));
        }

        let no_response = self.response.is_none();
        let no_request = self.request.is_none();

        if no_response && no_request {
            log::debug!(bytes_from = self.pos ; "GET {}", self.url);
            let request = new_request(&self.client, self.url.clone(), self.pos);
            self.request = Some((self.pos, request));
        }

        if let Some((_pos, request)) = self.request.as_mut() {
            match ready!(request.poll_unpin(cx)) {
                Ok(stream) => {
                    // put response stream
                    self.response = Some(stream);
                    self.request = None;
                }
                Err(err) => {
                    self.request = None;
                    return std::task::Poll::Ready(Err(std::io::Error::other(Box::new(err))));
                }
            }
        }

        let Some(response) = self.response.as_mut() else {
            panic!("response should be Some after polled")
        };

        let Some(stream_chunks) = ready!(response.poll_next_unpin(cx)) else {
            return std::task::Poll::Ready(Ok(()));
        };

        match stream_chunks {
            Ok(chunk) => {
                let size = chunk.len().min(buf.remaining());
                buf.put_slice(&chunk[..size]);
                self.pos += size as u64;
                if size < chunk.len() {
                    self.last_chunk = Some(chunk.slice(size..));
                }
                self.reset_retry();
                std::task::Poll::Ready(Ok(()))
            }
            Err(e) => {
                if self.retry_attempt == 0 {
                    return std::task::Poll::Ready(Err(std::io::Error::other(Box::new(e))));
                }

                if e.is_timeout() || e.status().is_some_and(|s| s.is_server_error()) {
                    log::warn!("timeout, retrying... attempts left: {}", self.retry_attempt);
                    self.retry_attempt -= 1;
                    self.response = None;
                    return self.poll_read(cx, buf);
                }

                std::task::Poll::Ready(Err(std::io::Error::other(Box::new(e))))
            }
        }
    }
}

impl AsyncSeek for HttpFile {
    fn start_seek(
        mut self: std::pin::Pin<&mut Self>,
        position: std::io::SeekFrom,
    ) -> std::io::Result<()> {
        if let Some(content_length) = self.content_length {
            let content_length = content_length.get();
            let effective_pos = match position {
                std::io::SeekFrom::Start(n) => n,
                std::io::SeekFrom::End(n) => {
                    content_length.checked_add_signed(n).ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid seek to end")
                    })?
                }
                std::io::SeekFrom::Current(n) => {
                    if n == 0 {
                        self.seek = Some(self.pos);
                        return Ok(());
                    }
                    self.pos.checked_add_signed(n).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "invalid seek to current",
                        )
                    })?
                }
            };
            if effective_pos > content_length {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid seek beyond end",
                ));
            }
            self.seek = Some(effective_pos);
            Ok(())
        } else {
            if matches!(position, std::io::SeekFrom::End(_)) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "cannot seek from end without known content length",
                ));
            }

            let effective_pos = match position {
                std::io::SeekFrom::Start(n) => n,
                std::io::SeekFrom::End(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "cannot seek from end without known content length",
                    ));
                }
                std::io::SeekFrom::Current(n) => {
                    if n == 0 {
                        self.seek = Some(self.pos);
                        return Ok(());
                    }
                    self.pos.checked_add_signed(n).ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "invalid seek to current",
                        )
                    })?
                }
            };
            self.seek = Some(effective_pos);
            Ok(())
        }
    }
    fn poll_complete(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        if self.seek == Some(self.pos) {
            self.seek = None;
            return std::task::Poll::Ready(Ok(self.pos));
        }

        let Some(seek_pos) = self.seek else {
            return std::task::Poll::Ready(Ok(self.pos));
        };

        // If seeking to or beyond EOF, just update position without making a request
        if let Some(content_length) = self.content_length {
            if seek_pos >= content_length.get() {
                self.pos = seek_pos;
                self.seek = None;
                self.request = None;
                self.response = None;
                self.last_chunk = None;
                return std::task::Poll::Ready(Ok(self.pos));
            }
        }

        if self.request.is_none() || self.request.as_ref().unwrap().0 != seek_pos {
            log::debug!(bytes_from = self.pos ; "GET {}", self.url);
            let request = new_request(&self.client, self.url.clone(), seek_pos);
            self.request = Some((seek_pos, request));
        }

        match ready!(self.request.as_mut().unwrap().1.poll_unpin(cx)) {
            Ok(stream) => {
                self.response = Some(stream);
                self.pos = seek_pos;
                self.seek = None;
                self.request = None;
                self.last_chunk = None;
                std::task::Poll::Ready(Ok(self.pos))
            }
            Err(err) => {
                self.request = None;
                std::task::Poll::Ready(Err(std::io::Error::other(Box::new(err))))
            }
        }
    }
}
