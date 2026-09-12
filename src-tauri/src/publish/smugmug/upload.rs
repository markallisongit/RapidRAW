//! Uploading one rendered image to SmugMug, retrying what is safe to retry,
//! and working out afterwards what landed when a failure left that unknown.
//!
//! Four details each break the upload on their own, and each is pinned by a
//! test below:
//!
//! 1. It goes to `upload.smugmug.com`, not the API host, and the signature
//!    covers that URL.
//! 2. The file is the raw request body — not multipart, not form-encoded — so
//!    the body contributes nothing to the OAuth signature and `Content-MD5` is
//!    what protects it.
//! 3. OAuth rides in the `Authorization` header. Query-string OAuth works for
//!    the rest of the API and fails opaquely here.
//! 4. `X-Smug-ImageUri` replaces an image rather than adding one. It is the
//!    whole mechanism behind republishing without duplicates.
//!
//! The file is buffered rather than streamed. The `reqwest` declarations do
//! not enable `stream`, and at three concurrent uploads of 5–25 MB JPEGs the
//! peak is ~75 MB — not worth editing two shared dependency lines for.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose};
use bytes::Bytes;
use futures::future::BoxFuture;
use md5::{Digest, Md5};
use reqwest::StatusCode;
use reqwest::header::HeaderValue;

use crate::publish::oauth1::{self, Credentials};
use crate::publish::smugmug::api::SmugMugApi;
use crate::publish::smugmug::auth::transport;
use crate::publish::smugmug::model::{UploadOutcome, parse_upload_response};
use crate::publish::{PublishError, PublishItem, RemoteContainerId, RemoteImageId};

/// Overridden only in tests, where a local mock server stands in.
pub const UPLOAD_URL: &str = "https://upload.smugmug.com/";

/// Every attempt, the first included.
const MAX_ATTEMPTS: u32 = 4;

const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_CEILING: Duration = Duration::from_secs(60);

/// A `Retry-After` longer than this is not waited out mid-batch: the image is
/// handed back as rate limited and the session decides what to do with it.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Connecting carries no payload, so it gets a fixed ceiling of its own; only
/// the transfer is sized from the file.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// How much of an error body is worth quoting back.
const ERROR_BODY_LIMIT: usize = 200;

/// Waits for the given time. Injected so backoff is tested without a clock.
pub type Sleep = Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

pub fn tokio_sleep() -> Sleep {
    Arc::new(|delay| Box::pin(tokio::time::sleep(delay)))
}

/// How long an upload may take before it is abandoned.
///
/// A fixed timeout is wrong in both directions when files vary 20× in size
/// and uplinks 100× in speed: long enough for a 25 MB file on a slow line is
/// minutes of staring at a dead connection on a fast one. So the timeout is
/// the time the file should take at the measured rate, with headroom, on top
/// of a floor for the request and the server's own processing.
#[derive(Debug, Clone)]
pub struct TimeoutPolicy {
    pub floor: Duration,
    pub ceiling: Duration,
    /// Used until an upload has completed and there is a real measurement.
    /// Deliberately pessimistic — roughly a 1 Mbit/s uplink — because a
    /// timeout that fires on a healthy upload causes the very ambiguity this
    /// module exists to resolve.
    pub assumed_bytes_per_sec: f64,
    /// Multiplier on the expected transfer time.
    pub headroom: f64,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            floor: Duration::from_secs(30),
            ceiling: Duration::from_secs(30 * 60),
            assumed_bytes_per_sec: 128.0 * 1024.0,
            headroom: 2.0,
        }
    }
}

impl TimeoutPolicy {
    /// Doubles with each retry: a timeout that has already fired once is
    /// evidence the estimate was low, and the same estimate would fire again.
    fn for_upload(&self, size: u64, bytes_per_sec: Option<f64>, retry: u32) -> Duration {
        let rate = bytes_per_sec.unwrap_or(self.assumed_bytes_per_sec).max(1.0);
        let transfer = Duration::from_secs_f64(size as f64 * self.headroom / rate);
        let base = (self.floor + transfer).min(self.ceiling);
        base.saturating_mul(1 << retry.min(16)).min(self.ceiling)
    }
}

/// Uploads to one account. One per session, so the throughput it measures
/// describes the connection the rest of the batch is using — including the
/// share of it that concurrent uploads leave.
pub struct SmugMugUploader {
    upload_url: String,
    client: reqwest::Client,
    creds: Credentials,
    sleep: Sleep,
    timeouts: TimeoutPolicy,
    /// Bytes per second, smoothed across completed uploads.
    throughput: Mutex<Option<f64>>,
}

impl SmugMugUploader {
    pub fn new(creds: Credentials) -> Result<Self, PublishError> {
        Self::with_config(UPLOAD_URL, creds, tokio_sleep(), TimeoutPolicy::default())
    }

    pub fn with_config(
        upload_url: impl Into<String>,
        creds: Credentials,
        sleep: Sleep,
        timeouts: TimeoutPolicy,
    ) -> Result<Self, PublishError> {
        // No client-wide timeout: each request sets its own from the file.
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(transport)?;
        Ok(Self {
            upload_url: upload_url.into(),
            client,
            creds,
            sleep,
            timeouts,
            throughput: Mutex::new(None),
        })
    }

    /// Uploads `item`, retrying 5xx and 429 with the same request id.
    ///
    /// Returns [`PublishError::Ambiguous`] rather than giving up with the last
    /// error whenever any attempt may have committed: the session must then
    /// [`reconcile`] rather than record a failure or retry blindly.
    /// `cancel` is checked before each attempt.
    pub async fn upload(
        &self,
        item: &PublishItem<'_>,
        cancel: &AtomicBool,
    ) -> Result<RemoteImageId, PublishError> {
        // Read once, hashed once: every retry sends the same bytes, and a
        // `Bytes` clone shares them rather than copying the file again.
        let body = Bytes::from(
            tokio::fs::read(item.file)
                .await
                .map_err(|e| PublishError::Io(format!("{}: {e}", item.file.display())))?,
        );
        let content_md5 = general_purpose::STANDARD.encode(Md5::digest(&body));
        let ambiguous = || PublishError::Ambiguous {
            file_name: item.file_name.clone(),
        };

        let mut maybe_committed = false;
        let mut retry = 0;
        loop {
            // After an attempt that may have landed, stopping is itself
            // ambiguous: only reconciliation can say whether it is there.
            if cancel.load(Ordering::Relaxed) {
                return Err(if maybe_committed {
                    ambiguous()
                } else {
                    PublishError::Cancelled
                });
            }

            let error = match self.attempt(item, &body, &content_md5, retry).await {
                Attempt::Uploaded(id) => return Ok(id),
                Attempt::Failed {
                    error,
                    maybe_committed: this_one,
                } => {
                    maybe_committed |= this_one;
                    error
                }
            };

            let retry_after = match &error {
                PublishError::RateLimited { retry_after } => *retry_after,
                _ => None,
            };
            let out_of_attempts = retry + 1 >= MAX_ATTEMPTS;
            let wait_too_long = retry_after.is_some_and(|after| after > MAX_RETRY_AFTER);
            if !error.is_retryable() || out_of_attempts || wait_too_long {
                return Err(if maybe_committed { ambiguous() } else { error });
            }

            let delay = retry_after.unwrap_or_else(|| backoff(retry));
            log::warn!(
                "SmugMug upload of {} failed on attempt {} ({error}); retrying in {delay:?}",
                item.file_name,
                retry + 1
            );
            (self.sleep)(delay).await;
            retry += 1;
        }
    }

    /// One signed POST. Classifies the outcome rather than merely mapping it
    /// to an error, because whether the server may have committed decides
    /// between a failure and an ambiguity.
    async fn attempt(
        &self,
        item: &PublishItem<'_>,
        body: &Bytes,
        content_md5: &str,
        retry: u32,
    ) -> Attempt {
        let headers = match upload_headers(item, content_md5, retry) {
            Ok(headers) => headers,
            Err(error) => return Attempt::refused(error),
        };
        let size = body.len() as u64;
        let timeout = self
            .timeouts
            .for_upload(size, self.measured_throughput(), retry);
        // The body is not part of the signature: only form-encoded bodies
        // are, and this one is raw bytes, which `Content-MD5` covers instead.
        let authorization = oauth1::authorization_header(
            "POST",
            &self.upload_url,
            &[],
            &self.creds,
            &oauth1::nonce(),
            oauth1::timestamp(),
        );

        let started = Instant::now();
        let sent = self
            .client
            .post(&self.upload_url)
            .headers(headers)
            .header("Authorization", authorization)
            .timeout(timeout)
            .body(body.clone())
            .send()
            .await;

        let response = match sent {
            Ok(response) => response,
            // Refused before a byte was sent: nothing can have landed.
            Err(error) if error.is_connect() => return Attempt::retryable(transport(error), false),
            // A timeout or a connection dropped mid-transfer can happen after
            // the server has stored the file.
            Err(error) => return Attempt::retryable(transport(error), true),
        };

        let status = response.status();
        let retry_after = retry_after(&response);
        let text = match response.text().await {
            Ok(text) => text,
            Err(error) => return Attempt::retryable(transport(error), true),
        };

        match status {
            status if status.is_success() => match parse_upload_response(&text) {
                Ok(UploadOutcome::Uploaded(id)) => {
                    self.record_throughput(size, started.elapsed());
                    Attempt::Uploaded(id)
                }
                Ok(UploadOutcome::Refused(detail)) => {
                    Attempt::refused(PublishError::Rejected(detail))
                }
                // A 2xx whose body cannot be read most likely did land. Not
                // retried — that risks a duplicate — but left for reconcile.
                Err(error) => Attempt::Failed {
                    error,
                    maybe_committed: true,
                },
            },
            // Refused at the door, before the upload was processed.
            StatusCode::TOO_MANY_REQUESTS => {
                Attempt::retryable(PublishError::RateLimited { retry_after }, false)
            }
            // The server failed somewhere in processing, which may have been
            // after the file was stored.
            status if status.is_server_error() => {
                Attempt::retryable(PublishError::Network(detail(status, &text)), true)
            }
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Attempt::refused(PublishError::NotAuthorised(detail(status, &text)))
            }
            status => Attempt::refused(PublishError::Rejected(detail(status, &text))),
        }
    }

    fn measured_throughput(&self) -> Option<f64> {
        self.throughput.lock().ok().and_then(|rate| *rate)
    }

    /// Smoothed so one upload that shared the line with a burst of others, or
    /// with a slow server, does not swing the next timeout on its own.
    fn record_throughput(&self, bytes: u64, elapsed: Duration) {
        let seconds = elapsed.as_secs_f64();
        if seconds <= 0.0 {
            return;
        }
        let sample = bytes as f64 / seconds;
        if let Ok(mut rate) = self.throughput.lock() {
            *rate = Some(match *rate {
                Some(previous) => previous * 0.7 + sample * 0.3,
                None => sample,
            });
        }
    }
}

/// How one attempt ended.
enum Attempt {
    Uploaded(RemoteImageId),
    Failed {
        error: PublishError,
        /// Whether the server may have stored the file despite the failure.
        maybe_committed: bool,
    },
}

impl Attempt {
    fn retryable(error: PublishError, maybe_committed: bool) -> Self {
        Self::Failed {
            error,
            maybe_committed,
        }
    }

    fn refused(error: PublishError) -> Self {
        Self::Failed {
            error,
            maybe_committed: false,
        }
    }
}

/// Exponential, capped, with jitter across the upper half of each step so
/// concurrent uploads that failed together do not retry together.
fn backoff(retry: u32) -> Duration {
    let step = BACKOFF_BASE
        .saturating_mul(1 << retry.min(16))
        .min(BACKOFF_CEILING);
    step.mul_f64(0.5 + rand::random::<f64>() * 0.5)
}

/// Only the delay-seconds form. The HTTP-date form is legal but not what
/// SmugMug sends, and a malformed value falls back to ordinary backoff.
fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get("Retry-After")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

fn detail(status: StatusCode, body: &str) -> String {
    let body = body.trim();
    let quoted = match body.char_indices().nth(ERROR_BODY_LIMIT) {
        Some((cut, _)) => format!("{}…", &body[..cut]),
        None => body.to_string(),
    };
    format!("HTTP {status}: {quoted}")
}

/// Everything but `Authorization`, which is signed per attempt.
fn upload_headers(
    item: &PublishItem<'_>,
    content_md5: &str,
    retry: u32,
) -> Result<reqwest::header::HeaderMap, PublishError> {
    let mut headers = reqwest::header::HeaderMap::new();
    let mut set = |name: &'static str, value: &str| -> Result<(), PublishError> {
        headers.insert(name, header_value(name, value)?);
        Ok(())
    };

    set("Content-MD5", content_md5)?;
    set("Content-Type", item.mime)?;
    set("X-Smug-Version", "v2")?;
    set("X-Smug-ResponseType", "JSON")?;
    set("X-Smug-Pretty", "false")?;
    set("X-Smug-AlbumUri", &item.container.0)?;
    set("X-Smug-FileName", &item.file_name)?;
    set("X-Smug-UploadRequestId", &item.request_id.to_string())?;
    set("X-Smug-RetryCount", &retry.to_string())?;
    if let Some(title) = &item.title {
        set("X-Smug-Title", title)?;
    }
    if let Some(caption) = &item.caption {
        set("X-Smug-Caption", caption)?;
    }
    if !item.keywords.is_empty() {
        set("X-Smug-Keywords", &item.keywords.join("; "))?;
    }
    if let Some(replaces) = &item.replaces {
        set("X-Smug-ImageUri", &replaces.0)?;
    }
    Ok(headers)
}

/// Metadata is user text, and a caption with a line break in it would
/// otherwise end the header early — or fail to build one at all. Control
/// characters become spaces; non-ASCII passes through as UTF-8 bytes.
fn header_value(name: &str, value: &str) -> Result<HeaderValue, PublishError> {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    HeaderValue::from_bytes(cleaned.trim().as_bytes())
        .map_err(|e| PublishError::Rejected(format!("cannot send {name} as a header: {e}")))
}

/// Asks the album what landed after one or more ambiguous uploads.
///
/// An expected item counts as landed when the album holds an image with the
/// same file name and the same size as the spooled file. What is returned is
/// safe to write to the state file; anything absent is to be uploaded again.
pub async fn reconcile(
    api: &SmugMugApi,
    container: &RemoteContainerId,
    expected: &[PublishItem<'_>],
) -> Result<Vec<(String, RemoteImageId)>, PublishError> {
    let listed = api.list_album_images(container).await?;

    let mut landed = Vec::new();
    for item in expected {
        let size = local_size(item.file).await?;
        if let Some(image) = listed
            .iter()
            .find(|image| image.file_name == item.file_name && image.size_bytes == size)
        {
            landed.push((item.file_name.clone(), image.image_uri.clone()));
        }
    }
    Ok(landed)
}

async fn local_size(file: &Path) -> Result<u64, PublishError> {
    tokio::fs::metadata(file)
        .await
        .map(|metadata| metadata.len())
        .map_err(|e| PublishError::Io(format!("{}: {e}", file.display())))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use serde_json::{Value, json};
    use tempfile::TempDir;
    use uuid::Uuid;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    use super::*;

    const ALBUM: &str = "/api/v2/album/AbCdEf";
    const IMAGE: &str = "/api/v2/image/XyZ123-0";

    fn test_creds() -> Credentials {
        Credentials {
            consumer_key: "consumer-key".into(),
            consumer_secret: "consumer-secret".into(),
            token: Some("access-token".into()),
            token_secret: Some("access-secret".into()),
        }
    }

    /// A spooled render on disk. The bytes include a CRLF pair and a `--`
    /// run, which is what a multipart boundary would be built from.
    struct Spooled {
        _dir: TempDir,
        file: PathBuf,
        bytes: Vec<u8>,
        container: RemoteContainerId,
    }

    fn spooled(file_name: &str) -> Spooled {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(file_name);
        let mut bytes = b"\xFF\xD8\xFF\xE0--boundary\r\n\r\n".to_vec();
        bytes.extend((0..4096u32).map(|n| (n % 251) as u8));
        std::fs::write(&file, &bytes).unwrap();
        Spooled {
            _dir: dir,
            file,
            bytes,
            container: RemoteContainerId(ALBUM.into()),
        }
    }

    fn item(spooled: &Spooled, replaces: Option<RemoteImageId>) -> PublishItem<'_> {
        PublishItem {
            file: &spooled.file,
            file_name: spooled.file.file_name().unwrap().to_str().unwrap().into(),
            mime: "image/jpeg",
            title: Some("Kirkjufell at dawn".into()),
            caption: None,
            keywords: vec![],
            container: &spooled.container,
            replaces,
            request_id: Uuid::new_v4(),
        }
    }

    /// Records every wait it is asked for and returns at once.
    fn recording_sleep() -> (Sleep, Arc<Mutex<Vec<Duration>>>) {
        let waits = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&waits);
        let sleep: Sleep = Arc::new(move |delay| {
            recorder.lock().unwrap().push(delay);
            Box::pin(async {})
        });
        (sleep, waits)
    }

    /// Generous enough that a local mock never trips it.
    fn relaxed() -> TimeoutPolicy {
        TimeoutPolicy {
            floor: Duration::from_secs(10),
            ceiling: Duration::from_secs(10),
            assumed_bytes_per_sec: 1e9,
            headroom: 1.0,
        }
    }

    fn uploader(server: &MockServer, sleep: Sleep, timeouts: TimeoutPolicy) -> SmugMugUploader {
        SmugMugUploader::with_config(format!("{}/", server.uri()), test_creds(), sleep, timeouts)
            .unwrap()
    }

    fn uploaded() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(
            json!({
                "stat": "ok",
                "method": "smugmug.images.upload",
                "Image": { "ImageUri": IMAGE, "AlbumImageUri": format!("{ALBUM}/image/XyZ123-0") }
            })
            .to_string(),
            "application/json",
        )
    }

    fn header<'r>(request: &'r Request, name: &str) -> Option<&'r str> {
        request
            .headers
            .get(name)
            .map(|value| value.to_str().expect("an ASCII header"))
    }

    fn not_cancelled() -> AtomicBool {
        AtomicBool::new(false)
    }

    #[tokio::test]
    async fn sends_the_file_as_a_raw_body_with_a_correct_content_md5() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(uploaded())
            .expect(1)
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234_edited.jpg");

        let (sleep, _) = recording_sleep();
        let id = uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &not_cancelled())
            .await
            .unwrap();

        assert_eq!(id, RemoteImageId(IMAGE.into()));
        let requests = server.received_requests().await.unwrap();
        let request = &requests[0];
        assert_eq!(
            request.body, spooled.bytes,
            "the body is the file, byte for byte"
        );
        let digest = Md5::digest(&spooled.bytes);
        assert_eq!(
            header(request, "content-md5"),
            Some(general_purpose::STANDARD.encode(digest).as_str())
        );
        assert_eq!(
            header(request, "content-length"),
            Some(spooled.bytes.len().to_string().as_str())
        );
        assert_eq!(header(request, "content-type"), Some("image/jpeg"));
        assert_eq!(header(request, "x-smug-albumuri"), Some(ALBUM));
        assert_eq!(
            header(request, "x-smug-filename"),
            Some("DSC_1234_edited.jpg")
        );
        assert_eq!(header(request, "x-smug-title"), Some("Kirkjufell at dawn"));
        assert_eq!(header(request, "x-smug-responsetype"), Some("JSON"));
        assert_eq!(header(request, "x-smug-version"), Some("v2"));
        assert_eq!(
            header(request, "x-smug-caption"),
            None,
            "an absent caption is omitted"
        );
    }

    #[tokio::test]
    async fn oauth_is_in_the_authorization_header_and_absent_from_the_query() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(uploaded())
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");

        let (sleep, _) = recording_sleep();
        uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &not_cancelled())
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        let request = &requests[0];
        let authorization = header(request, "authorization").expect("the upload is signed");
        assert!(authorization.starts_with("OAuth "), "{authorization}");
        assert!(
            authorization.contains("oauth_signature="),
            "{authorization}"
        );
        assert!(
            authorization.contains("oauth_token=\"access-token\""),
            "{authorization}"
        );
        assert_eq!(
            request.url.query(),
            None,
            "query-string OAuth fails opaquely on the upload host: {}",
            request.url
        );
    }

    #[tokio::test]
    async fn replacing_sets_x_smug_imageuri_and_creating_omits_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(uploaded())
            .expect(2)
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");
        let (sleep, _) = recording_sleep();
        let uploader = uploader(&server, sleep, relaxed());

        uploader
            .upload(&item(&spooled, None), &not_cancelled())
            .await
            .unwrap();
        uploader
            .upload(
                &item(&spooled, Some(RemoteImageId(IMAGE.into()))),
                &not_cancelled(),
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(header(&requests[0], "x-smug-imageuri"), None);
        assert_eq!(header(&requests[1], "x-smug-imageuri"), Some(IMAGE));
    }

    #[tokio::test]
    async fn retries_a_500_then_succeeds_with_the_same_request_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(uploaded())
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");
        let item = item(&spooled, None);

        let (sleep, waits) = recording_sleep();
        let id = uploader(&server, sleep, relaxed())
            .upload(&item, &not_cancelled())
            .await
            .unwrap();

        assert_eq!(id, RemoteImageId(IMAGE.into()));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2);
        let expected_id = item.request_id.to_string();
        let ids: Vec<_> = requests
            .iter()
            .map(|r| header(r, "x-smug-uploadrequestid"))
            .collect();
        assert_eq!(
            ids,
            [Some(expected_id.as_str()), Some(expected_id.as_str())]
        );
        let counts: Vec<_> = requests
            .iter()
            .map(|r| header(r, "x-smug-retrycount"))
            .collect();
        assert_eq!(counts, [Some("0"), Some("1")]);
        assert_eq!(
            waits.lock().unwrap().len(),
            1,
            "one backoff between the attempts"
        );
        // A signature is single-use, so each attempt is signed afresh.
        assert_ne!(
            header(&requests[0], "authorization"),
            header(&requests[1], "authorization")
        );
    }

    #[tokio::test]
    async fn honours_retry_after_on_429() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "2"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(uploaded())
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");

        let (sleep, waits) = recording_sleep();
        uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &not_cancelled())
            .await
            .unwrap();

        let waits = waits.lock().unwrap();
        assert_eq!(waits.len(), 1);
        assert!(
            waits[0] >= Duration::from_secs(2),
            "waited only {:?}",
            waits[0]
        );
    }

    #[tokio::test]
    async fn gives_up_after_four_attempts_and_reports_ambiguous_on_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(uploaded().set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");
        let item = item(&spooled, None);
        let impatient = TimeoutPolicy {
            floor: Duration::from_millis(50),
            ceiling: Duration::from_millis(150),
            assumed_bytes_per_sec: 1e9,
            headroom: 1.0,
        };

        let (sleep, waits) = recording_sleep();
        let result = uploader(&server, sleep, impatient)
            .upload(&item, &not_cancelled())
            .await;

        assert!(
            matches!(&result, Err(PublishError::Ambiguous { file_name }) if file_name == "DSC_1234.jpg"),
            "a timeout may have committed, so it is neither a failure nor a success: {result:?}"
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 4, "four attempts, then stop");
        let expected_id = item.request_id.to_string();
        assert!(
            requests
                .iter()
                .all(|r| header(r, "x-smug-uploadrequestid") == Some(expected_id.as_str())),
            "every attempt carries the same request id"
        );
        let counts: Vec<_> = requests
            .iter()
            .map(|r| header(r, "x-smug-retrycount"))
            .collect();
        assert_eq!(counts, [Some("0"), Some("1"), Some("2"), Some("3")]);
        assert_eq!(
            waits.lock().unwrap().len(),
            3,
            "no wait after the last attempt"
        );
    }

    #[tokio::test]
    async fn a_refused_upload_is_not_retried() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_string("Invalid album"))
            .expect(1)
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");

        let (sleep, waits) = recording_sleep();
        let result = uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &not_cancelled())
            .await;

        assert!(
            matches!(result, Err(PublishError::Rejected(_))),
            "{result:?}"
        );
        assert!(waits.lock().unwrap().is_empty());
    }

    /// A 200 carrying `"stat": "fail"` is a refusal, not a success.
    #[tokio::test]
    async fn a_failed_stat_in_a_200_is_a_refusal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"stat": "fail", "code": 7, "message": "invalid album"}"#,
                "application/json",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");

        let (sleep, _) = recording_sleep();
        let result = uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &not_cancelled())
            .await;

        assert!(
            matches!(result, Err(PublishError::Rejected(_))),
            "{result:?}"
        );
    }

    /// 429 is refused before the upload is processed, so running out of
    /// attempts on it is a plain failure, not a reason to reconcile.
    #[tokio::test]
    async fn rate_limiting_that_never_lifts_is_not_ambiguous() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429))
            .expect(4)
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");

        let (sleep, _) = recording_sleep();
        let result = uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &not_cancelled())
            .await;

        assert!(
            matches!(result, Err(PublishError::RateLimited { .. })),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn a_cancelled_upload_sends_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(uploaded())
            .expect(0)
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");

        let (sleep, _) = recording_sleep();
        let result = uploader(&server, sleep, relaxed())
            .upload(&item(&spooled, None), &AtomicBool::new(true))
            .await;

        assert!(matches!(result, Err(PublishError::Cancelled)), "{result:?}");
    }

    #[tokio::test]
    async fn a_line_break_in_a_caption_cannot_end_the_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(uploaded())
            .mount(&server)
            .await;
        let spooled = spooled("DSC_1234.jpg");
        let mut item = item(&spooled, None);
        item.caption = Some("First light.\r\nX-Smug-Hidden: true".into());
        item.keywords = vec!["iceland".into(), "dawn".into()];

        let (sleep, _) = recording_sleep();
        uploader(&server, sleep, relaxed())
            .upload(&item, &not_cancelled())
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            header(&requests[0], "x-smug-caption"),
            Some("First light.  X-Smug-Hidden: true")
        );
        assert_eq!(header(&requests[0], "x-smug-hidden"), None);
        assert_eq!(
            header(&requests[0], "x-smug-keywords"),
            Some("iceland; dawn")
        );
    }

    #[test]
    fn the_timeout_grows_with_file_size_and_shrinks_with_measured_throughput() {
        let policy = TimeoutPolicy::default();
        const MB: u64 = 1024 * 1024;

        let small = policy.for_upload(5 * MB, None, 0);
        let large = policy.for_upload(25 * MB, None, 0);
        let large_on_a_fast_line = policy.for_upload(25 * MB, Some(10.0 * MB as f64), 0);

        assert!(large > small, "{large:?} vs {small:?}");
        assert!(
            large_on_a_fast_line < large,
            "{large_on_a_fast_line:?} vs {large:?}"
        );
        assert!(large_on_a_fast_line >= policy.floor);
        assert!(policy.for_upload(25 * MB, Some(1.0), 0) <= policy.ceiling);
        assert!(
            policy.for_upload(5 * MB, None, 1) > small,
            "a retry after a timeout waits longer"
        );
    }

    #[test]
    fn backoff_grows_and_stays_within_its_ceiling() {
        for retry in 0..10 {
            let delay = backoff(retry);
            let step = BACKOFF_BASE.saturating_mul(1 << retry).min(BACKOFF_CEILING);
            assert!(
                delay >= step / 2 && delay <= step,
                "retry {retry}: {delay:?}"
            );
        }
    }

    fn images_page(images: &[(&str, usize, &str)]) -> String {
        let images: Vec<Value> = images
            .iter()
            .map(|(name, size, uri)| {
                json!({
                    "FileName": name,
                    "ArchivedSize": size,
                    "Uri": format!("{ALBUM}/image/{}", uri.rsplit('/').next().unwrap()),
                    "Uris": { "Image": { "Uri": uri } }
                })
            })
            .collect();
        json!({
            "Response": { "AlbumImage": images, "Pages": { "Total": 1, "Start": 1 } },
            "Code": 200
        })
        .to_string()
    }

    async fn mount_album(server: &MockServer, body: String) -> SmugMugApi {
        Mock::given(method("GET"))
            .and(path(format!("{ALBUM}!images")))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(server)
            .await;
        SmugMugApi::with_base_url(server.uri(), test_creds()).unwrap()
    }

    #[tokio::test]
    async fn reconcile_finds_an_image_that_landed_despite_a_timeout() {
        let server = MockServer::start().await;
        let spooled = spooled("DSC_1234.jpg");
        let api = mount_album(
            &server,
            images_page(&[
                ("DSC_0001.jpg", 5_000_000, "/api/v2/image/Other-0"),
                ("DSC_1234.jpg", spooled.bytes.len(), IMAGE),
            ]),
        )
        .await;

        let landed = reconcile(&api, &spooled.container, &[item(&spooled, None)])
            .await
            .unwrap();

        assert_eq!(
            landed,
            vec![("DSC_1234.jpg".to_string(), RemoteImageId(IMAGE.into()))]
        );
    }

    #[tokio::test]
    async fn reconcile_reports_an_image_that_did_not_land() {
        let server = MockServer::start().await;
        let landed_file = spooled("DSC_1234.jpg");
        let missing_file = spooled("DSC_1235.jpg");
        let api = mount_album(
            &server,
            images_page(&[
                ("DSC_1234.jpg", landed_file.bytes.len(), IMAGE),
                // Same name, different size: a partial or older upload, not this render.
                (
                    "DSC_1235.jpg",
                    missing_file.bytes.len() + 1,
                    "/api/v2/image/Stale-0",
                ),
            ]),
        )
        .await;

        let landed = reconcile(
            &api,
            &landed_file.container,
            &[item(&landed_file, None), item(&missing_file, None)],
        )
        .await
        .unwrap();

        assert_eq!(
            landed,
            vec![("DSC_1234.jpg".to_string(), RemoteImageId(IMAGE.into()))],
            "an image absent from the result is re-queued"
        );
    }

    fn live_var(name: &str) -> String {
        std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set"))
    }

    /// The one check wiremock cannot make: that `X-Smug-ImageUri` really
    /// replaces in place on SmugMug rather than adding a second copy. Uploads
    /// one JPEG, uploads it again as a replacement, and counts the copies.
    /// Leaves one image behind in the album.
    ///
    /// ```text
    /// SMUGMUG_CONSUMER_KEY=… SMUGMUG_CONSUMER_SECRET=… \
    /// SMUGMUG_ACCESS_TOKEN=… SMUGMUG_ACCESS_SECRET=… \
    /// SMUGMUG_ALBUM_URI=/api/v2/album/… SMUGMUG_TEST_JPEG=/path/to/photo.jpg \
    ///     cargo test --lib -- --ignored --nocapture live_replace
    /// ```
    #[tokio::test]
    #[ignore = "live: needs SmugMug credentials, an album URI and a JPEG"]
    async fn live_replace_does_not_duplicate() {
        let creds = Credentials {
            consumer_key: live_var("SMUGMUG_CONSUMER_KEY"),
            consumer_secret: live_var("SMUGMUG_CONSUMER_SECRET"),
            token: Some(live_var("SMUGMUG_ACCESS_TOKEN")),
            token_secret: Some(live_var("SMUGMUG_ACCESS_SECRET")),
        };
        let container = RemoteContainerId(live_var("SMUGMUG_ALBUM_URI"));
        let file = PathBuf::from(live_var("SMUGMUG_TEST_JPEG"));
        let file_name = format!("rapidraw-live-{}.jpg", Uuid::new_v4().simple());
        let uploader = SmugMugUploader::new(creds.clone()).unwrap();
        let upload = |replaces: Option<RemoteImageId>| PublishItem {
            file: &file,
            file_name: file_name.clone(),
            mime: "image/jpeg",
            title: Some("RapidRAW replace check".into()),
            caption: None,
            keywords: vec![],
            container: &container,
            replaces,
            // A fresh id: the replacement is a new upload, not a retry.
            request_id: Uuid::new_v4(),
        };

        let created = uploader
            .upload(&upload(None), &not_cancelled())
            .await
            .expect("the first upload should land");
        println!("created: {}", created.0);
        let replaced = uploader
            .upload(&upload(Some(created.clone())), &not_cancelled())
            .await
            .expect("the replacement should land");
        println!("replaced: {}", replaced.0);

        let api = SmugMugApi::new(creds).unwrap();
        let copies: Vec<_> = api
            .list_album_images(&container)
            .await
            .unwrap()
            .into_iter()
            .filter(|image| image.file_name == file_name)
            .collect();
        assert_eq!(copies.len(), 1, "replace added a copy: {copies:?}");
    }
}
