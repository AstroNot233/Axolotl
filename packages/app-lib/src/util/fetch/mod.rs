//! Functions for fetching information from the Internet
mod client;
mod file;
mod model;
mod native;
mod policy;
mod probe;
mod progress;
mod request;
mod routes;
mod segment;
mod segmented;
mod tail;
mod throttle;
mod transfer;
pub(crate) use self::client::*;
pub use self::client::{
    INSECURE_REQWEST_CLIENT, build_proxied_client, configured_client,
};
pub(crate) use self::file::*;
pub use self::file::{
    cleanup_stale_partial_downloads, copy, read_json, sha1_async,
    sha1_file_async, sha1_file_cancellable, write, write_cached_icon,
};
pub(crate) use self::model::*;
pub use self::model::{
    ContentValidation, DownloadMeta, DownloadReason, DownloadRequest,
    DownloadResult, DownloadRoute, DownloadRouteSource, FetchProgressFn,
    Integrity, ProxyPolicy, ResourceClass,
};
pub(crate) use self::native::*;
pub(crate) use self::policy::*;
pub(crate) use self::probe::*;
pub(crate) use self::progress::*;
pub(crate) use self::request::*;
pub use self::request::{
    fetch, fetch_advanced, fetch_advanced_with_client, fetch_json,
    fetch_json_nonempty, fetch_official, post_json,
};
pub use self::routes::resolve_download_routes_for;
pub(crate) use self::routes::*;
pub(crate) use self::segment::*;
pub(crate) use self::segmented::*;
pub(crate) use self::tail::*;
pub(crate) use self::throttle::*;
pub use self::throttle::{
    FetchSemaphore, IoSemaphore, clear_download_dns_host_override,
    set_download_dns_host_override,
};
pub use self::transfer::download_to_path;
pub(crate) use self::transfer::*;
pub use super::download::constants::DOWNLOAD_META_HEADER;
#[cfg(test)]
use super::download::constants::MAX_REDIRECT_LOCATION_BYTES;
use super::download::modrinth_redirect::is_official_redirect as is_official_modrinth_cdn_redirect;
#[cfg(test)]
use super::download::modrinth_redirect::repair_official_redirect as repair_official_cdn_redirect;
use super::download::retired_sources;
use super::download_manager::{DownloadSpeedTracker, SpeedSnapshot};
use super::io;
use crate::ErrorKind;
use crate::util::download::constants::{
    DOWNLOAD_SCHEDULING_GRACE, FILE_TRANSFER_FIRST_BYTE_TIMEOUT,
    H2_FALLBACK_TTL, MAX_SEGMENT_CONCURRENCY, METADATA_HEDGE_DELAY,
    MIN_SEGMENT_SIZE, REASSIGNABLE_FIRST_BYTE_TIMEOUT, RETRY_AFTER_CAP,
    ROUTE_PROBE_BYTES, SEGMENT_EXPANSION_INTERVAL,
    SEGMENT_EXPANSION_SAMPLE_COUNT, SEGMENTED_DOWNLOAD_THRESHOLD,
};
#[cfg(test)]
use crate::util::download::constants::{
    FETCH_127_0_0_1, FETCH_LOCALHOST, HTTP_SCHEME,
};
use crate::util::download::constants::{
    FILE_VALIDATION_CONCURRENCY, ROUTE_PROBE_CONCURRENCY,
};
use bytes::Bytes;
use parking_lot::Mutex;
use reqwest::{Method, StatusCode, header};
use serde::Deserialize;
use std::error::Error;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{self, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{Notify, Semaphore};
use url::Url;

#[cfg(test)]
fn is_safe_redirect_location(location: &str) -> bool {
    location.len() <= MAX_REDIRECT_LOCATION_BYTES && location.is_ascii()
}

use super::download::integrity::{
    is_integrity_error, verify_computed_integrity, verify_file,
};
#[cfg(test)]
use super::download::native_request::byte_range_header_value;
#[cfg(test)]
use super::download::route_health::resource_family;
use super::download::route_health::{
    ROUTE_HEALTH, ResourceFamily, RouteHealth,
};

#[cfg(test)]
fn create_initial_ranges(size: u64, count: usize) -> Vec<DownloadRange> {
    create_remaining_ranges(size, 0, count)
}

#[cfg(test)]
pub(crate) async fn download_endpoint_fixture(
    request: DownloadRequest,
    destination: &Path,
    semaphore: &FetchSemaphore,
) -> crate::Result<DownloadResult> {
    download_to_path_inner(request, destination, semaphore, None).await
}

#[cfg(test)]
tokio::task_local! { static BENCHMARK_TIMING: (); }

#[cfg(test)]
pub(crate) async fn download_benchmark_fixture(
    request: DownloadRequest,
    destination: &Path,
    semaphore: &FetchSemaphore,
) -> crate::Result<DownloadResult> {
    BENCHMARK_TIMING
        .scope(
            (),
            download_to_path_inner(request, destination, semaphore, None),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn post_verification_path_replacement_never_receives_a_trusted_proof()
    {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("staged");
        tokio::fs::write(&path, b"original").await.unwrap();
        let integrity =
            Integrity::sha1(sha1_smol::Sha1::from(b"original").hexdigest());
        verify_file(&path, &integrity).await.unwrap();
        let replacement = directory.path().join("replacement");
        tokio::fs::write(&replacement, b"modified").await.unwrap();
        finalize_download(&replacement, &path).await.unwrap();
        let result = DownloadResult {
            path: path.clone(),
            url: "https://proof.invalid/file".into(),
            source: DownloadRouteSource::Official,
            size: 8,
            attempts: 1,
            fallback_count: 0,
            verified_sha1: None,
            verified_sha512: None,
            verified_file: None,
        };
        let result = attach_verified_integrity(result, &integrity).await;
        assert!(result.verified_file.is_none());
        assert!(verify_file(&path, &integrity).await.is_err());
    }

    #[tokio::test]
    async fn failed_checkpoint_ranges_restart_full_download_but_io_and_cancel_keep_checkpoints()
     {
        use super::super::download::h2_download::{
            H2DownloadFailure, H2DownloadOutcome,
        };
        let directory = tempfile::tempdir().unwrap();
        let part = directory.path().join("file.part");
        let destination = directory.path().join("file");
        let journal = super::super::download::range_journal::path(&part);
        let request = DownloadRequest::new(
            "https://resume.invalid/file",
            ResourceClass::Other,
        )
        .with_integrity(Integrity::sha1("expected"));
        let route = official_route(&request.url, ResourceClass::Other);
        for failure in [
            H2DownloadFailure::RangeUnsupported,
            H2DownloadFailure::Protocol,
            H2DownloadFailure::Io,
        ] {
            tokio::fs::write(&part, b"sparse-part").await.unwrap();
            tokio::fs::write(&journal, b"checkpoint").await.unwrap();
            let result = resume_checkpointed_download_with(
                &request,
                &[route.clone()],
                &part,
                |_| async {
                    Ok(Some(H2DownloadOutcome::Fallback {
                        failure,
                        preserve_partial: failure
                            != H2DownloadFailure::RangeUnsupported,
                    }))
                },
            )
            .await;
            if failure == H2DownloadFailure::Io {
                assert!(result.is_err());
                assert!(part.exists() && journal.exists());
            } else {
                assert!(result.unwrap().is_none());
                assert!(!part.exists() && !journal.exists());
                tokio::fs::write(&part, b"complete full response")
                    .await
                    .unwrap();
                finalize_download(&part, &destination).await.unwrap();
                assert_eq!(
                    tokio::fs::read(&destination).await.unwrap(),
                    b"complete full response"
                );
            }
        }
        assert!(
            resume_checkpointed_download_with(
                &request,
                &[route],
                &part,
                |_| async { Ok(Some(H2DownloadOutcome::Canceled)) }
            )
            .await
            .is_err()
        );
        assert!(part.exists() && journal.exists());
    }
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    static RANGE_SPLITTING_TEST_LOCK: LazyLock<AsyncMutex<()>> =
        LazyLock::new(|| AsyncMutex::new(()));
    static AUTO_SOURCE_TEST_LOCK: LazyLock<std::sync::Mutex<()>> =
        LazyLock::new(|| std::sync::Mutex::new(()));
    static H2_FALLBACK_TEST_LOCK: LazyLock<std::sync::Mutex<()>> =
        LazyLock::new(|| std::sync::Mutex::new(()));

    #[derive(Clone, Copy, Debug, Default)]
    struct RangeServerBehavior {
        wrong_content_range: bool,
        ignore_range: bool,
        slow_body: bool,
        fail_first_range: bool,
        stall_first_range: bool,
        vary_mod_validators: bool,
        vary_range_validators: bool,
    }

    async fn spawn_range_server(
        data: Arc<Vec<u8>>,
        behavior: RangeServerBehavior,
    ) -> (
        String,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = requests.clone();
        let normal_requests = Arc::new(AtomicUsize::new(0));
        let normal_request_count = normal_requests.clone();
        let failed_range = Arc::new(AtomicBool::new(false));
        let stalled_range = Arc::new(AtomicBool::new(false));
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let data = data.clone();
                let requests = request_count.clone();
                let normal_requests = normal_request_count.clone();
                let failed_range = Arc::clone(&failed_range);
                let stalled_range = Arc::clone(&stalled_range);
                tokio::spawn(async move {
                    requests.fetch_add(1, Ordering::Relaxed);
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    loop {
                        let Ok(read) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if request
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n")
                        {
                            break;
                        }
                    }
                    let request =
                        String::from_utf8_lossy(&request).to_ascii_lowercase();
                    let requested_range = request
                        .lines()
                        .find_map(|line| line.strip_prefix("range: bytes="));
                    let (mut headers, body) = if let Some(range) =
                        requested_range.filter(|_| !behavior.ignore_range)
                    {
                        let Some((start, end)) = range.split_once('-') else {
                            return;
                        };
                        let Ok(start) = start.parse::<u64>() else {
                            return;
                        };
                        let end = if end.is_empty() {
                            data.len() as u64 - 1
                        } else {
                            let Ok(end) = end.parse::<u64>() else {
                                return;
                            };
                            end
                        };
                        let body = &data[start as usize..=end as usize];
                        let reported_start = if behavior.wrong_content_range {
                            start.saturating_add(1)
                        } else {
                            start
                        };
                        (
                            format!(
                                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {reported_start}-{end}/{}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n",
                                body.len(),
                                data.len(),
                            ),
                            body,
                        )
                    } else {
                        if requested_range.is_none() {
                            normal_requests.fetch_add(1, Ordering::Relaxed);
                        }
                        (
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"fixture\"\r\nConnection: close\r\n\r\n",
                                data.len(),
                            ),
                            &data[..],
                        )
                    };
                    if let Some(range) = requested_range
                        && (behavior.vary_range_validators
                            || (behavior.vary_mod_validators
                                && request.lines().next().is_some_and(
                                    |line| line.contains(".jar "),
                                )))
                    {
                        headers = headers.replace(
                            "\"fixture\"",
                            &format!("\"fixture-{range}\""),
                        );
                    }
                    if stream.write_all(headers.as_bytes()).await.is_err() {
                        return;
                    }
                    if requested_range.is_some()
                        && behavior.stall_first_range
                        && !stalled_range.swap(true, Ordering::Relaxed)
                    {
                        tokio::time::sleep(DOWNLOAD_SCHEDULING_GRACE).await;
                        return;
                    }
                    if requested_range.is_some()
                        && behavior.fail_first_range
                        && !failed_range.swap(true, Ordering::Relaxed)
                    {
                        let midpoint = body.len() / 2;
                        let _ = stream.write_all(&body[..midpoint]).await;
                        return;
                    }
                    for chunk in body.chunks(64 * 1024) {
                        if stream.write_all(chunk).await.is_err() {
                            return;
                        }
                        if behavior.slow_body {
                            tokio::time::sleep(time::Duration::from_millis(
                                100,
                            ))
                            .await;
                        }
                    }
                });
            }
        });
        (
            format!("http://{address}/file"),
            requests,
            normal_requests,
            handle,
        )
    }

    async fn spawn_stream_server(
        chunks: Vec<(Duration, Vec<u8>)>,
        content_length: usize,
        hold_open: bool,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let chunks = Arc::new(chunks);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let chunks = Arc::clone(&chunks);
                tokio::spawn(async move {
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    loop {
                        let Ok(read) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if request
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n")
                        {
                            break;
                        }
                    }
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
                    );
                    if stream.write_all(headers.as_bytes()).await.is_err() {
                        return;
                    }
                    for (delay, chunk) in chunks.iter() {
                        tokio::time::sleep(*delay).await;
                        if stream.write_all(chunk).await.is_err() {
                            return;
                        }
                    }
                    if hold_open {
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                });
            }
        });
        (format!("http://{address}/file"), handle)
    }

    async fn spawn_json_server(
        body: String,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let requests = Arc::clone(&request_count);
                let body = body.clone();
                tokio::spawn(async move {
                    requests.fetch_add(1, Ordering::Relaxed);
                    let mut buffer = [0_u8; 1024];
                    let _ = stream.read(&mut buffer).await;
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(headers.as_bytes()).await;
                    let _ = stream.write_all(body.as_bytes()).await;
                });
            }
        });
        (
            format!("http://{address}/tag/game_version"),
            requests,
            handle,
        )
    }

    struct HttpFixture {
        status_line: &'static str,
        extra_headers: &'static str,
        body: Vec<u8>,
        response_delay: Duration,
    }

    impl HttpFixture {
        fn new(status_line: &'static str, body: impl Into<Vec<u8>>) -> Self {
            Self {
                status_line,
                extra_headers: "",
                body: body.into(),
                response_delay: Duration::ZERO,
            }
        }

        fn with_headers(mut self, headers: &'static str) -> Self {
            self.extra_headers = headers;
            self
        }

        fn with_delay(mut self, delay: Duration) -> Self {
            self.response_delay = delay;
            self
        }
    }

    async fn spawn_http_fixture(
        fixture: HttpFixture,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let status_line = fixture.status_line.to_string();
        let extra_headers = fixture.extra_headers.to_string();
        let body = Arc::new(fixture.body);
        let response_delay = fixture.response_delay;
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let requests = Arc::clone(&request_count);
                let status_line = status_line.clone();
                let extra_headers = extra_headers.clone();
                let body = Arc::clone(&body);
                tokio::spawn(async move {
                    requests.fetch_add(1, Ordering::Relaxed);
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    loop {
                        let Ok(read) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if request
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n")
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(response_delay).await;
                    let response = format!(
                        "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
                        body.len(),
                    );
                    if stream.write_all(response.as_bytes()).await.is_ok() {
                        let _ = stream.write_all(&body).await;
                    }
                });
            }
        });
        (format!("http://{address}/file"), requests, handle)
    }

    fn direct_test_route(
        url: String,
        source: DownloadRouteSource,
    ) -> DownloadRoute {
        DownloadRoute {
            url,
            source,
            is_mirror: source != DownloadRouteSource::Official,
            allow_sensitive_headers: source == DownloadRouteSource::Official,
            supports_range: false,
            proxy: ProxyPolicy::Direct,
        }
    }

    async fn spawn_redirect_server(
        location: String,
        response_delay: Duration,
    ) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let request_count = Arc::clone(&requests);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let location = location.clone();
                let requests = Arc::clone(&request_count);
                tokio::spawn(async move {
                    requests.fetch_add(1, Ordering::Relaxed);
                    let mut request = Vec::new();
                    let mut buffer = [0_u8; 1024];
                    loop {
                        let Ok(read) = stream.read(&mut buffer).await else {
                            return;
                        };
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                        if request
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n")
                        {
                            break;
                        }
                    }
                    tokio::time::sleep(response_delay).await;
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{address}/redirect"), requests, handle)
    }

    async fn test_download(
        url: &str,
        destination: &Path,
        expected_size: u64,
    ) -> crate::Result<DownloadResult> {
        download_to_path(
            DownloadRequest::new(url, ResourceClass::Other)
                .with_integrity(Integrity::default().with_size(expected_size)),
            destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
    }
    #[test]
    fn modrinth_requests_are_classified_for_logging() {
        assert_eq!(
            modrinth_request_kind("https://api.modrinth.com/v2/project"),
            Some("API")
        );
        assert_eq!(
            modrinth_request_kind(
                "https://cdn.modrinth.com/data/project/version/file.jar"
            ),
            Some("CDN")
        );
        assert_eq!(
            modrinth_request_kind(
                "https://cdn-alt.modrinth.com/data/project/version/file.jar"
            ),
            None
        );
        assert_eq!(modrinth_request_kind("https://example.com/file.jar"), None);
    }

    #[test]
    fn modrinth_cdn_routes_use_the_current_host() {
        for host in ["cdn.modrinth.com", "cdn-alt.modrinth.com"] {
            let url = format!(
                "https://{host}/data/project/version/file.jar?download=1"
            );
            let routes = resolve_download_routes_for(
                &url,
                ResourceClass::Modrinth,
                crate::state::DownloadSourceMode::OfficialOnly,
            );
            assert_eq!(routes.len(), 1);
            assert_eq!(
                routes[0].url,
                "https://cdn.modrinth.com/data/project/version/file.jar?download=1"
            );
        }
    }

    #[test]
    fn modrinth_download_url_recognition_excludes_retired_hosts() {
        assert!(!is_official_modrinth_download_url(
            "https://cdn-alt.modrinth.com/data/project/version/file.jar"
        ));
        assert!(is_official_modrinth_download_url(
            "https://cdn.modrinth.com/data/project/version/file.jar"
        ));
        assert!(is_official_modrinth_download_url(
            "https://api.modrinth.com/v2/project/abc"
        ));
        assert!(!is_official_modrinth_download_url(
            "https://example.com/file.jar"
        ));
    }

    #[test]
    fn cache_busted_urls_preserve_existing_query() {
        assert_eq!(
            cache_busted_download_url(
                "https://cdn-alt.modrinth.com/data/project/version/file.jar?download=1",
                2,
            ),
            "https://cdn-alt.modrinth.com/data/project/version/file.jar?download=1&axolotl_retry=2"
        );
        assert_eq!(
            cache_busted_download_url(
                "https://cdn-alt.modrinth.com/data/project/version/file.jar",
                1,
            ),
            "https://cdn-alt.modrinth.com/data/project/version/file.jar?axolotl_retry=1"
        );
        // A previous buster is replaced, not stacked.
        assert_eq!(
            cache_busted_download_url(
                "https://cdn-alt.modrinth.com/data/project/version/file.jar?axolotl_retry=1&download=1",
                3,
            ),
            "https://cdn-alt.modrinth.com/data/project/version/file.jar?download=1&axolotl_retry=3"
        );
    }

    #[test]
    fn retired_modrinth_candidates_are_deduplicated_in_every_source_mode() {
        for mode in [
            crate::state::DownloadSourceMode::Auto,
            crate::state::DownloadSourceMode::OfficialOnly,
            crate::state::DownloadSourceMode::OfficialPreferred,
            crate::state::DownloadSourceMode::MirrorPreferred,
        ] {
            let request = DownloadRequest::new(
                "https://cdn-alt.modrinth.com/data/pack.mrpack",
                ResourceClass::Modpack,
            )
            .with_candidate_urls([
                "https://cdn.modrinth.com/data/pack.mrpack",
                "https://cdn-alt.modrinth.com/data/other.jar?x=%2b",
            ]);
            let routes = build_download_routes(&request, mode);
            assert_eq!(routes.len(), 2);
            assert_eq!(
                routes[0].url,
                "https://cdn.modrinth.com/data/pack.mrpack"
            );
            assert_eq!(
                routes[1].url,
                "https://cdn.modrinth.com/data/other.jar?x=%2b"
            );
            assert!(!routes.iter().any(|route| route.url.contains("cdn-alt")));
            assert_eq!(routes[0].source, DownloadRouteSource::Official);
            assert!(is_official_modrinth_download_url(&routes[0].url));
            assert_eq!(modrinth_request_kind(&routes[0].url), Some("CDN"));
            assert_eq!(
                infer_resource_class(&routes[0].url),
                ResourceClass::Modrinth
            );
        }
        assert_eq!(
            modrinth_request_kind(
                "https://cdn.modrinth.com.evil.example/file.jar"
            ),
            None
        );
    }

    #[test]
    fn size_mismatch_is_lenient_when_a_hash_matches() {
        let matching = ComputedIntegrity {
            size: 122_777,
            sha1: Some("abc".to_string()),
            ..ComputedIntegrity::default()
        };
        let hashed = Integrity {
            size: Some(122_778),
            sha1: Some("abc".to_string()),
            ..Integrity::default()
        };
        assert!(
            verify_computed_integrity(&hashed, &matching).is_ok(),
            "a hash match must win over an off-by-a-few-bytes size claim"
        );
        let size_only = Integrity {
            size: Some(122_778),
            ..Integrity::default()
        };
        assert!(
            verify_computed_integrity(&size_only, &matching).is_err(),
            "without a hash the size claim is still enforced"
        );
    }

    #[test]
    fn identifies_hash_integrity_failures_without_treating_size_errors_as_hash_failures()
     {
        let hash_error: crate::Error = ErrorKind::OtherError(
            "Incorrect sha1 hash for download: expected != actual".to_string(),
        )
        .into();
        assert!(is_integrity_error(&hash_error));

        let typed_hash_error: crate::Error =
            ErrorKind::HashError("expected".to_string(), "actual".to_string())
                .into();
        assert!(is_integrity_error(&typed_hash_error));

        let size_error: crate::Error = ErrorKind::OtherError(
            "Incorrect size for download: 10 != 20".to_string(),
        )
        .into();
        assert!(!is_integrity_error(&size_error));
    }

    #[test]
    fn segmented_download_flag_is_buildable_per_request() {
        let request = DownloadRequest::new(
            "https://example.com/a.jar",
            ResourceClass::Modpack,
        );
        assert!(request.allow_segmented_download);
        let request = request.with_segmented_download(false);
        assert!(!request.allow_segmented_download);
    }

    #[test]
    fn batch_requests_can_disable_http1_ranges_without_disabling_h2() {
        let request = DownloadRequest::new(
            "https://example.com/content.jar",
            ResourceClass::Modpack,
        )
        .with_http1_segmented_download(false);

        assert!(request.allow_segmented_download);
        assert!(!request.allow_http1_segmented_download);
    }

    #[test]
    fn batch_policy_splits_ten_megabyte_items_with_a_known_hash() {
        for (size, segmented) in
            [(9_999_999, false), (10_000_000, true), (139_963_795, true)]
        {
            let request = DownloadRequest::new(
                "https://example.com/content.jar",
                ResourceClass::Modpack,
            )
            .with_integrity(Integrity::sha1("expected").with_size(size))
            .with_batch_download_policy();
            assert!(request.allow_segmented_download);
            assert_eq!(request.allow_http1_segmented_download, segmented);
        }
        let request = DownloadRequest::new(
            "https://example.com/content.jar",
            ResourceClass::Modpack,
        )
        .with_integrity(Integrity::default().with_size(10_000_000))
        .with_batch_download_policy();
        assert!(!request.allow_http1_segmented_download);
    }

    #[test]
    fn verified_file_classes_share_range_growth_and_recovery() {
        for resource in [
            ResourceClass::MinecraftAsset,
            ResourceClass::MinecraftLibrary,
            ResourceClass::Loader,
            ResourceClass::Java,
            ResourceClass::Modrinth,
            ResourceClass::CurseForge,
            ResourceClass::Modpack,
            ResourceClass::Other,
        ] {
            let request =
                DownloadRequest::new("https://example.com/file", resource)
                    .with_integrity(
                        Integrity::sha1("expected").with_size(16 * 1024 * 1024),
                    );
            assert!(
                request.prefers_independent_ranges(16 * 1024 * 1024),
                "{resource:?}"
            );
            assert_eq!(
                requested_range_floor(
                    &request,
                    16 * 1024 * 1024,
                    16 * 1024 * 1024,
                    64
                ),
                32
            );
            assert_eq!(
                requested_range_floor(
                    &request,
                    16 * 1024 * 1024,
                    MIN_SEGMENT_SIZE * 3,
                    64
                ),
                3
            );
            assert_eq!(
                requested_range_floor(
                    &request,
                    16 * 1024 * 1024,
                    16 * 1024 * 1024,
                    2
                ),
                2
            );
            assert!(can_recover_file_ranges(&request, MIN_SEGMENT_SIZE * 2));
            assert!(
                !request
                    .clone()
                    .with_http1_segmented_download(false)
                    .supports_range_recovery()
            );
            assert!(
                !request
                    .clone()
                    .with_segmented_download(false)
                    .supports_range_recovery()
            );
            assert!(
                !request
                    .with_integrity(Integrity::default())
                    .supports_range_recovery()
            );
        }
        let metadata = DownloadRequest::new(
            "https://example.com/metadata",
            ResourceClass::Metadata,
        )
        .with_integrity(
            Integrity::sha1("expected").with_size(16 * 1024 * 1024),
        );
        assert!(!metadata.supports_range_recovery());
    }

    #[test]
    fn h2_range_concurrency_is_explicit_per_request() {
        let request = DownloadRequest::new(
            "https://example.com/pack.mrpack",
            ResourceClass::Modpack,
        );
        assert_eq!(request.h2_range_concurrency, None);
        assert_eq!(
            request.with_h2_range_concurrency(8).h2_range_concurrency,
            Some(8)
        );
    }

    #[tokio::test]
    async fn java_h2_probe_discovers_size_before_selecting_independent_ranges()
    {
        let size = 16 * 1024 * 1024;
        let request = DownloadRequest::new(
            "https://java-size-probe.invalid/runtime.zip",
            ResourceClass::Java,
        )
        .with_integrity(Integrity::sha1("verified-at-completion"));
        let mut route = direct_test_route(
            request.url.clone(),
            DownloadRouteSource::Official,
        );
        route.supports_range = true;
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut server = h2::server::handshake(server_io).await.unwrap();
            let (request, mut respond) =
                server.accept().await.unwrap().unwrap();
            assert_eq!(request.headers()[header::RANGE], "bytes=0-0");
            let response = http::Response::builder()
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes 0-0/{size}"))
                .header(header::CONTENT_LENGTH, 1)
                .body(())
                .unwrap();
            respond
                .send_response(response, false)
                .unwrap()
                .send_data(Bytes::from_static(&[31]), true)
                .unwrap();
            while server.accept().await.is_some() {}
        });
        let (sender, driver) = h2::client::handshake(client_io).await.unwrap();
        let connection = super::super::download::h2_pool::SharedH2Connection::cache_for_test(&route, sender).await;
        let driver = super::super::download::h2_pool::spawn_connection_driver(
            &connection,
            driver,
            Duration::from_secs(60),
        );
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("runtime.zip");
        let part = suffixed_path(&destination, ".part");
        let semaphore = FetchSemaphore(Semaphore::new(64));
        let result = try_h2_download(
            &request,
            route,
            super::super::download::native::NativeH2Policy {
                allow_cold_connection: true,
                abort_if_slow: true,
                expected_speed: None,
            },
            &destination,
            &part,
            &semaphore,
        )
        .await
        .unwrap();
        assert!(matches!(result, H2AttemptResult::Fallback {
            prefer_http1: true,
            discovered_size: Some(discovered),
            failed_nonofficial: None,
        } if discovered == size));
        assert!(!destination.exists());
        assert!(!connection.is_dead());
        assert!(!connection.is_retiring());
        assert_eq!(semaphore.0.available_permits(), 64);
        driver.abort();
        server.abort();
    }

    #[tokio::test]
    async fn warm_h2_archive_hint_cannot_bypass_large_file_connections() {
        let request = DownloadRequest::new(
            "https://warm-archive-policy.invalid/pack.mrpack",
            ResourceClass::Modpack,
        )
        .with_integrity(Integrity::sha1("expected").with_size(9_999_999))
        .with_h2_range_concurrency(16);
        let mut route = direct_test_route(
            request.url.clone(),
            DownloadRouteSource::Official,
        );
        route.supports_range = true;
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let mut server = h2::server::handshake(server_io).await.unwrap();
            while server.accept().await.is_some() {}
        });
        let (sender, driver) = h2::client::handshake(client_io).await.unwrap();
        let connection = super::super::download::h2_pool::SharedH2Connection::cache_for_test(&route, sender).await;
        let driver = super::super::download::h2_pool::spawn_connection_driver(
            &connection,
            driver,
            Duration::from_secs(60),
        );
        let directory = tempfile::tempdir().unwrap();
        let part = directory.path().join("archive.part");
        let routes = [route.clone()];
        assert!(
            select_h2_download_route(&request, &routes, &part)
                .await
                .is_some()
        );
        for size in [10_000_000, 27_137_250, 100_855_703] {
            let large = request
                .clone()
                .with_integrity(Integrity::sha1("expected").with_size(size));
            assert!(
                select_h2_download_route(&large, &routes, &part)
                    .await
                    .is_none()
            );
            let explicitly_shared = large.with_http1_segmented_download(false);
            assert!(
                select_h2_download_route(&explicitly_shared, &routes, &part)
                    .await
                    .is_some()
            );
        }
        assert!(!connection.is_retiring());
        driver.abort();
        server.abort();
    }

    #[tokio::test]
    async fn file_download_times_out_when_a_successful_response_stalls() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let (url, server) = spawn_stream_server(Vec::new(), 4, true).await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("stalled.bin");

        let error = test_download(&url, &destination, 4).await.unwrap_err();

        assert!(matches!(
            error.raw.as_ref(),
            ErrorKind::NetworkError(_) | ErrorKind::FetchError(_)
        ));
        assert!(!destination.exists());
        assert!(!suffixed_path(&destination, ".part").exists());
        server.abort();
    }

    #[tokio::test]
    async fn file_download_allows_data_that_keeps_arriving_slowly() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let chunks = (0..4)
            .map(|byte| (Duration::from_millis(100), vec![byte]))
            .collect();
        let (url, server) = spawn_stream_server(chunks, 4, false).await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("slow.bin");

        let result = test_download(&url, &destination, 4).await.unwrap();

        assert_eq!(result.size, 4);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), [0, 1, 2, 3]);
        server.abort();
    }

    #[tokio::test]
    async fn file_download_completes_normally() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let (url, server) = spawn_stream_server(
            vec![(Duration::ZERO, b"done".to_vec())],
            4,
            false,
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("complete.bin");

        let result = test_download(&url, &destination, 4).await.unwrap();

        assert_eq!(result.size, 4);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"done");
        server.abort();
    }

    #[tokio::test]
    async fn segmentation_disabled_downloads_use_a_single_connection() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let (url, requests, normal_requests, server) =
            spawn_range_server(data, RangeServerBehavior::default()).await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("batch.bin");

        let result = download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other)
                .with_segmented_download(false)
                .with_integrity(Integrity::default().with_size(size as u64)),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, size as u64);
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert_eq!(normal_requests.load(Ordering::Relaxed), 1);
        server.abort();
    }

    #[tokio::test]
    async fn file_download_drops_a_missing_route_before_later_rounds() {
        let (missing_url, missing_requests, missing_server) =
            spawn_http_fixture(HttpFixture::new("404 Not Found", Vec::new()))
                .await;
        let (fallback_url, fallback_requests, fallback_server) =
            spawn_http_fixture(HttpFixture::new("200 OK", b"done")).await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("fallback.bin");

        let result = download_to_path(
            DownloadRequest::new(&missing_url, ResourceClass::Other)
                .with_candidate_urls([fallback_url])
                .with_integrity(Integrity::default().with_size(4)),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, 4);
        assert_eq!(missing_requests.load(Ordering::Relaxed), 1);
        assert_eq!(fallback_requests.load(Ordering::Relaxed), 1);
        missing_server.abort();
        fallback_server.abort();
    }

    #[tokio::test]
    async fn file_download_bounds_server_error_retries_to_three_rounds() {
        let (url, requests, server) = spawn_http_fixture(HttpFixture::new(
            "503 Service Unavailable",
            Vec::new(),
        ))
        .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("unavailable.bin");
        let request_url = format!("{url}?token=secret#fragment");

        let error = test_download(&request_url, &destination, 4)
            .await
            .unwrap_err();
        let diagnostic = error.to_string();

        assert_eq!(requests.load(Ordering::Relaxed), 3);
        assert!(diagnostic.contains("after 3/3 attempts"));
        assert!(diagnostic.contains("category=http"));
        assert!(diagnostic.contains("status=Some(503)"));
        assert!(!diagnostic.contains("secret"));
        assert!(!diagnostic.contains("fragment"));
        assert!(!destination.exists());
        server.abort();
    }

    #[tokio::test]
    async fn rate_limited_route_cools_down_and_switches_without_waiting() {
        let (limited_url, limited_requests, limited_server) =
            spawn_http_fixture(
                HttpFixture::new("429 Too Many Requests", Vec::new())
                    .with_headers("Retry-After: 60\r\n"),
            )
            .await;
        let (fallback_url, fallback_requests, fallback_server) =
            spawn_http_fixture(HttpFixture::new("200 OK", b"done")).await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("rate-limit-fallback.bin");
        let started = Instant::now();

        let result = download_to_path(
            DownloadRequest::new(&limited_url, ResourceClass::Other)
                .with_candidate_urls([fallback_url])
                .with_integrity(Integrity::default().with_size(4)),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, 4);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(limited_requests.load(Ordering::Relaxed), 1);
        assert_eq!(fallback_requests.load(Ordering::Relaxed), 1);
        limited_server.abort();
        fallback_server.abort();
    }

    #[tokio::test]
    async fn metadata_hedging_returns_the_first_valid_response() {
        let (primary_url, primary_requests, primary_server) =
            spawn_http_fixture(
                HttpFixture::new("200 OK", br#"{"source":"primary"}"#)
                    .with_headers("Content-Type: application/json\r\n")
                    .with_delay(Duration::from_millis(300)),
            )
            .await;
        let (secondary_url, secondary_requests, secondary_server) =
            spawn_http_fixture(
                HttpFixture::new("200 OK", br#"{"source":"secondary"}"#)
                    .with_headers("Content-Type: application/json\r\n"),
            )
            .await;
        let routes = [
            direct_test_route(primary_url, DownloadRouteSource::Official),
            direct_test_route(secondary_url, DownloadRouteSource::Mcim),
        ];
        let validate = |bytes: &Bytes| -> crate::Result<()> {
            serde_json::from_slice::<serde_json::Value>(bytes)?;
            Ok(())
        };

        let bytes = fetch_hedged_metadata(
            &routes,
            ResourceClass::Metadata,
            None,
            None,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            &INSECURE_REQWEST_CLIENT,
            &validate,
        )
        .await
        .unwrap();

        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({ "source": "secondary" })
        );
        assert_eq!(primary_requests.load(Ordering::Relaxed), 1);
        assert_eq!(secondary_requests.load(Ordering::Relaxed), 1);
        primary_server.abort();
        secondary_server.abort();
    }

    #[tokio::test]
    async fn metadata_hedging_rejects_a_fast_invalid_primary() {
        let (primary_url, primary_requests, primary_server) =
            spawn_http_fixture(
                HttpFixture::new("200 OK", b"not-json")
                    .with_headers("Content-Type: application/json\r\n"),
            )
            .await;
        let (secondary_url, secondary_requests, secondary_server) =
            spawn_http_fixture(
                HttpFixture::new("200 OK", br#"{"valid":true}"#)
                    .with_headers("Content-Type: application/json\r\n"),
            )
            .await;
        let routes = [
            direct_test_route(primary_url, DownloadRouteSource::Official),
            direct_test_route(secondary_url, DownloadRouteSource::Mcim),
        ];
        let validate = |bytes: &Bytes| -> crate::Result<()> {
            serde_json::from_slice::<serde_json::Value>(bytes)?;
            Ok(())
        };

        let bytes = fetch_hedged_metadata(
            &routes,
            ResourceClass::Metadata,
            None,
            None,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            &INSECURE_REQWEST_CLIENT,
            &validate,
        )
        .await
        .unwrap();

        assert_eq!(bytes, Bytes::from_static(br#"{"valid":true}"#));
        assert_eq!(primary_requests.load(Ordering::Relaxed), 1);
        assert_eq!(secondary_requests.load(Ordering::Relaxed), 1);
        primary_server.abort();
        secondary_server.abort();
    }

    #[tokio::test]
    async fn metadata_hedging_does_not_start_a_loser_for_a_fast_primary() {
        let (primary_url, primary_requests, primary_server) =
            spawn_http_fixture(
                HttpFixture::new("200 OK", br#"{"valid":true}"#)
                    .with_headers("Content-Type: application/json\r\n"),
            )
            .await;
        let (secondary_url, secondary_requests, secondary_server) =
            spawn_http_fixture(
                HttpFixture::new("200 OK", br#"{"valid":false}"#)
                    .with_headers("Content-Type: application/json\r\n"),
            )
            .await;
        let routes = [
            direct_test_route(primary_url, DownloadRouteSource::Official),
            direct_test_route(secondary_url, DownloadRouteSource::Mcim),
        ];
        let validate = |bytes: &Bytes| -> crate::Result<()> {
            serde_json::from_slice::<serde_json::Value>(bytes)?;
            Ok(())
        };

        let _ = fetch_hedged_metadata(
            &routes,
            ResourceClass::Metadata,
            None,
            None,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            &INSECURE_REQWEST_CLIENT,
            &validate,
        )
        .await
        .unwrap();
        tokio::time::sleep(METADATA_HEDGE_DELAY * 2).await;

        assert_eq!(primary_requests.load(Ordering::Relaxed), 1);
        assert_eq!(secondary_requests.load(Ordering::Relaxed), 0);
        primary_server.abort();
        secondary_server.abort();
    }

    #[tokio::test]
    async fn xml_metadata_hedging_bypasses_slow_or_invalid_official_responses()
    {
        #[derive(Deserialize)]
        struct XmlVersion {
            version: String,
        }
        for (body, delay) in [
            (
                b"<metadata><version>old</version></metadata>".as_slice(),
                Duration::from_secs(2),
            ),
            (b"<html>upstream error</html>".as_slice(), Duration::ZERO),
        ] {
            let (primary_url, primary_requests, primary_server) =
                spawn_http_fixture(
                    HttpFixture::new("200 OK", body).with_delay(delay),
                )
                .await;
            let valid = b"<metadata><version>21.1.200</version></metadata>";
            let (secondary_url, secondary_requests, secondary_server) =
                spawn_http_fixture(HttpFixture::new(
                    "200 OK",
                    valid.as_slice(),
                ))
                .await;
            let routes = [
                direct_test_route(primary_url, DownloadRouteSource::Official),
                direct_test_route(secondary_url, DownloadRouteSource::Bmclapi),
            ];
            let validate = |bytes: &Bytes| -> crate::Result<()> {
                quick_xml::de::from_reader::<_, XmlVersion>(bytes.as_ref())
                    .map(|_| ())
                    .map_err(|error| {
                        ErrorKind::OtherError(error.to_string()).as_error()
                    })
            };
            let bytes = tokio::time::timeout(
                Duration::from_secs(1),
                fetch_hedged_metadata(
                    &routes,
                    ResourceClass::Loader,
                    None,
                    None,
                    &FetchSemaphore(Semaphore::new(2)),
                    &INSECURE_REQWEST_CLIENT,
                    &validate,
                ),
            )
            .await
            .unwrap()
            .unwrap();
            let parsed: XmlVersion =
                quick_xml::de::from_reader(bytes.as_ref()).unwrap();
            assert_eq!(parsed.version, "21.1.200");
            assert_eq!(primary_requests.load(Ordering::Relaxed), 1);
            assert_eq!(secondary_requests.load(Ordering::Relaxed), 1);
            primary_server.abort();
            secondary_server.abort();
        }
    }

    #[tokio::test]
    async fn typed_metadata_fetch_validates_xml_without_initializing_application_state()
     {
        #[derive(Deserialize)]
        struct XmlVersion {
            version: String,
        }
        let (url, requests, server) = spawn_http_fixture(HttpFixture::new(
            "200 OK",
            b"<metadata><version>21.1.200</version></metadata>".as_slice(),
        ))
        .await;
        let clients = DownloadClients::build(
            &crate::util::proxy::ProxyConfig {
                mode: crate::util::proxy::ProxyMode::None,
                ..Default::default()
            },
            false,
            false,
        )
        .unwrap();
        let parsed = super::super::download::proxy_context::with_snapshot(
            clients,
            fetch_metadata(
                &url,
                |bytes| {
                    quick_xml::de::from_reader::<_, XmlVersion>(bytes.as_ref())
                        .map_err(|error| {
                            ErrorKind::OtherError(error.to_string()).as_error()
                        })
                },
                &FetchSemaphore(Semaphore::new(2)),
            ),
        )
        .await
        .unwrap();
        assert_eq!(parsed.version, "21.1.200");
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        server.abort();
    }

    #[tokio::test]
    async fn canceling_a_file_download_drops_the_response_read_promptly() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let (url, server) = spawn_stream_server(Vec::new(), 4, true).await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("canceled.bin");
        let task =
            tokio::spawn(
                async move { test_download(&url, &destination, 4).await },
            );

        tokio::time::sleep(Duration::from_millis(50)).await;
        task.abort();
        let join_error = tokio::time::timeout(Duration::from_millis(100), task)
            .await
            .expect("canceled download should stop promptly")
            .unwrap_err();

        assert!(join_error.is_cancelled());
        server.abort();
    }

    #[test]
    fn log_urls_keep_route_but_remove_credentials_and_fragments() {
        assert_eq!(
            sanitize_url_for_log(
                "https://mod.mcimirror.top/data/file.jar?X-Amz-Credential=secret&X-Amz-Signature=signature#fragment"
            ),
            "https://mod.mcimirror.top/data/file.jar"
        );
        assert_eq!(
            sanitize_url_for_log("not-a-url?token=secret#fragment"),
            "not-a-url"
        );
        assert_eq!(
            sanitize_url_for_log(
                "https://username:password@example.com/file.jar?token=secret"
            ),
            "https://example.com/file.jar"
        );
    }

    #[test]
    fn auto_source_health_is_scoped_by_resource_family() {
        let _guard = AUTO_SOURCE_TEST_LOCK.lock().unwrap();
        let previous_health = std::mem::take(&mut *ROUTE_HEALTH.lock());
        let manifest_url =
            "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
        let manifest_routes = resolve_download_routes_for(
            manifest_url,
            ResourceClass::Metadata,
            crate::state::DownloadSourceMode::Auto,
        );
        assert_eq!(manifest_routes[0].source, DownloadRouteSource::Official);

        record_route_failure(
            &manifest_routes[0],
            ResourceClass::Metadata,
            None,
        );
        let modrinth_routes = resolve_download_routes_for(
            "https://api.modrinth.com/v2/tag/game_version",
            ResourceClass::Modrinth,
            crate::state::DownloadSourceMode::Auto,
        );
        assert_eq!(modrinth_routes[0].source, DownloadRouteSource::Official);

        *ROUTE_HEALTH.lock() = previous_health;
    }

    #[test]
    fn disabled_tianpao_routes_leave_official_cdn_available() {
        let _guard = AUTO_SOURCE_TEST_LOCK.lock().unwrap();
        let previous_health = std::mem::take(&mut *ROUTE_HEALTH.lock());
        let url = "https://cdn-alt.modrinth.com/data/project/versions/version/file.jar";
        let mut routes = explicit_mirror_routes(url, ResourceClass::Modrinth);
        routes.push(official_route(url, ResourceClass::Modrinth));
        assert!(routes.iter().all(|route| {
            !Url::parse(&route.url)
                .is_ok_and(|url| retired_sources::is_retired(&url))
        }));
        let official = routes
            .iter()
            .find(|route| is_official_route(route))
            .unwrap();
        let key = route_health_key(official, ResourceClass::Modrinth).unwrap();
        ROUTE_HEALTH.lock().insert(
            key,
            RouteHealth {
                success_samples: 3,
                throughput_bps: Some(100_000_000.0),
                ..RouteHealth::default()
            },
        );

        order_auto_routes(&mut routes, ResourceClass::Modrinth);

        assert_eq!(routes[0].source, DownloadRouteSource::Official);
        *ROUTE_HEALTH.lock() = previous_health;
    }

    #[test]
    fn tianpao_routes_restore_official_sources_in_all_modes_and_skip_unknown_candidates()
     {
        for mode in [
            crate::state::DownloadSourceMode::Auto,
            crate::state::DownloadSourceMode::OfficialOnly,
            crate::state::DownloadSourceMode::OfficialPreferred,
            crate::state::DownloadSourceMode::MirrorPreferred,
        ] {
            for (input, official) in [
                (
                    "https://mod.tianpao.top/data/p/file.jar",
                    "https://cdn.modrinth.com/data/p/file.jar",
                ),
                (
                    "https://mod.tianpao.top/files/1/2/file.jar",
                    "https://edge.forgecdn.net/files/1/2/file.jar",
                ),
                (
                    "https://mod.tianpao.top/media/attachments/icon.png",
                    "https://media.forgecdn.net/attachments/icon.png",
                ),
            ] {
                let request =
                    DownloadRequest::new(input, ResourceClass::Modpack)
                        .with_candidate_urls([
                            official,
                            "https://mod.tianpao.top/unknown",
                        ]);
                let routes = build_download_routes(&request, mode);
                assert_eq!(routes.len(), 1);
                assert_eq!(routes[0].url, official);
                assert_eq!(routes[0].source, DownloadRouteSource::Official);
            }
            assert!(
                resolve_download_routes_for(
                    "https://mod.tianpao.top/unknown",
                    ResourceClass::Modpack,
                    mode
                )
                .is_empty()
            );
            let request = DownloadRequest::new(
                "https://mod.tianpao.top/unknown",
                ResourceClass::Modpack,
            )
            .with_candidate_urls(["https://cdn.modrinth.com/data/file.jar"]);
            assert_eq!(
                build_download_routes(&request, mode)[0].url,
                "https://cdn.modrinth.com/data/file.jar"
            );
        }
    }

    #[tokio::test]
    async fn tianpao_unknown_downloads_preserve_existing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("saved.bin");
        tokio::fs::write(&path, b"existing").await.unwrap();
        let error = download_to_path(
            DownloadRequest::new(
                "https://mod.tianpao.top/unknown",
                ResourceClass::Modpack,
            ),
            &path,
            &FetchSemaphore(Semaphore::new(1)),
            None,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("retired"));
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"existing");
    }

    #[tokio::test]
    async fn tianpao_metadata_redirects_are_blocked_before_following() {
        let (url, requests, server) = spawn_http_fixture(
            HttpFixture::new("302 Found", Vec::new())
                .with_headers("Location: https://mod.tianpao.top/unknown\r\n"),
        )
        .await;
        let client = reqwest_client_builder().no_proxy().build().unwrap();
        let error = client.get(url).send().await.unwrap_err();
        assert!(error.is_redirect());
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        server.abort();
    }

    #[test]
    fn modrinth_cdn_redirects_only_fall_back_to_official_cdn() {
        assert!(is_official_modrinth_cdn_redirect(Some(
            "https://cdn.modrinth.com/data/project/versions/version/file.jar"
        )));
        assert!(is_official_modrinth_cdn_redirect(Some(
            "https://cdn-alt.modrinth.com/data/project/versions/version/file.jar"
        )));
        assert!(is_official_modrinth_cdn_redirect(Some(
            "https://CDN.MODRINTH.COM/data/project/versions/version/file.jar"
        )));
        assert!(!is_official_modrinth_cdn_redirect(Some(
            "https://cache.mcimirror.top/data/project/versions/version/file.jar"
        )));
        assert!(!is_official_modrinth_cdn_redirect(Some(
            "https://cdn.modrinth.com.evil.example/file.jar"
        )));
        assert!(!is_official_modrinth_cdn_redirect(Some(
            "https://cdn.modrinth.com@evil.example/file.jar"
        )));
        assert!(!is_official_modrinth_cdn_redirect(Some(
            "https://cdn-alt.modrinth.com/\u{4e0b}\u{8f7d}/file.jar"
        )));
        assert!(!is_official_modrinth_cdn_redirect(None));
    }

    #[test]
    fn redirect_locations_are_bounded_ascii_values() {
        assert!(is_safe_redirect_location("/download/file.jar"));
        assert!(!is_safe_redirect_location("/\u{4e0b}\u{8f7d}/file.jar"));
        assert!(!is_safe_redirect_location(
            &"a".repeat(MAX_REDIRECT_LOCATION_BYTES + 1)
        ));
    }

    #[test]
    fn malformed_official_cdn_redirects_reuse_the_original_encoded_path() {
        let original = Url::parse(
            "https://cdn-alt.modrinth.com/data/project/versions/version/%E9%87%91%E5%90%88%E6%AC%A2_1.21%2B.zip",
        )
        .unwrap();
        for host in ["cdn-alt.modrinth.com", "cdn.modrinth.com"] {
            let redirect = Url::parse(&format!(
                "https://{host}/data/project/versions/version/\u{91c8}\u{91c8}.zip",
            ))
            .unwrap();

            let repaired = repair_official_cdn_redirect(
                &original,
                &redirect,
                &format!("https://{host}/data/project/versions/version/\u{91c8}\u{91c8}.zip"),
            )
            .unwrap();

            assert_eq!(
                repaired.as_str(),
                format!(
                    "https://cdn.modrinth.com/data/project/versions/version/%E9%87%91%E5%90%88%E6%AC%A2_1.21%2B.zip"
                )
            );
        }
    }

    #[test]
    fn mrpack_urls_are_detected_without_query_string() {
        assert!(is_mrpack_url(
            "https://cdn.modrinth.com/data/project/version/pack.MRPACK?download=1"
        ));
        assert!(!is_mrpack_url(
            "https://cdn.modrinth.com/data/project/version/mod.jar"
        ));
    }

    #[test]
    fn fetch_retries_use_short_jittered_backoff() {
        let cases = [
            (1, Duration::from_millis(85), Duration::from_millis(115)),
            (2, Duration::from_millis(255), Duration::from_millis(345)),
            (3, Duration::from_millis(637), Duration::from_millis(863)),
            (4, Duration::from_millis(637), Duration::from_millis(863)),
        ];
        for (attempt, minimum, maximum) in cases {
            let delay = fetch_retry_delay(attempt);
            assert!(delay >= minimum, "attempt {attempt}: {delay:?}");
            assert!(delay <= maximum, "attempt {attempt}: {delay:?}");
        }
    }

    #[test]
    fn retry_after_is_allowed_up_to_its_own_cap() {
        assert_eq!(
            clamp_failure_cooldown(Duration::from_secs(30)),
            Duration::from_secs(30),
        );
        assert_eq!(
            clamp_failure_cooldown(Duration::from_secs(3600)),
            RETRY_AFTER_CAP
        );
    }

    #[tokio::test]
    async fn finalize_download_moves_a_part_file_into_a_new_destination() {
        let directory = tempfile::tempdir().unwrap();
        let part = directory.path().join("file.part");
        let destination = directory.path().join("file.bin");
        tokio::fs::write(&part, b"downloaded").await.unwrap();

        finalize_download(&part, &destination).await.unwrap();

        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"downloaded");
        assert!(!part.exists(), "the part file should be consumed");
    }

    #[tokio::test]
    async fn finalize_download_replaces_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let part = directory.path().join("file.part");
        let destination = directory.path().join("file.bin");
        tokio::fs::write(&part, b"complete").await.unwrap();
        tokio::fs::write(&destination, b"partial").await.unwrap();

        finalize_download(&part, &destination).await.unwrap();

        assert_eq!(tokio::fs::read(&destination).await.unwrap(), b"complete");
        assert!(!part.exists(), "the part file should be consumed");
    }

    #[tokio::test]
    async fn finalize_download_keeps_the_destination_when_the_part_file_is_missing()
     {
        let directory = tempfile::tempdir().unwrap();
        let part = directory.path().join("missing.part");
        let destination = directory.path().join("file.bin");
        tokio::fs::write(&destination, b"previous").await.unwrap();

        finalize_download(&part, &destination).await.unwrap_err();

        assert_eq!(
            tokio::fs::read(&destination).await.unwrap(),
            b"previous",
            "a failed commit must not remove the existing destination"
        );
    }

    #[tokio::test]
    async fn finalize_download_aborts_when_the_flush_fails() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("file.bin");
        tokio::fs::write(&destination, b"previous").await.unwrap();
        // A directory cannot be opened for writing, so the flush fails for a
        // reason other than a missing file.
        let part = directory.path().join("unflushable.part");
        tokio::fs::create_dir(&part).await.unwrap();

        let error = finalize_download(&part, &destination).await.unwrap_err();

        assert!(
            !format!("{error}").is_empty(),
            "a failed flush must surface an error"
        );
        assert_eq!(
            tokio::fs::read(&destination).await.unwrap(),
            b"previous",
            "a failed flush must not replace the destination"
        );
    }

    #[test]
    fn h2_fallback_outlives_immediate_retry_cleanup() {
        let _guard = H2_FALLBACK_TEST_LOCK.lock();
        let authority = "h2-cooldown.example:443";
        record_authority_h2_failure(authority);
        assert!(authority_uses_http1_fallback(authority));
        let key = crate::util::download::proxy_context::authority_key(
            authority,
            ProxyPolicy::System,
        );
        let remaining = H2_FALLBACK_AUTHORITIES
            .lock()
            .get(&key)
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap();
        assert!(remaining <= H2_FALLBACK_TTL);
        assert!(remaining >= time::Duration::from_secs(4 * 60));
        H2_FALLBACK_AUTHORITIES.lock().remove(&key);
    }

    #[test]
    fn global_download_dns_override_can_be_registered_and_cleared() {
        set_download_dns_host_override(
            "request-host.test",
            "resolver-target.test",
        )
        .unwrap();
        assert_eq!(
            DOWNLOAD_DNS_RESOLVER
                .host_override("request-host.test")
                .as_deref(),
            Some("resolver-target.test"),
        );
        clear_download_dns_host_override("request-host.test").unwrap();
        assert!(
            DOWNLOAD_DNS_RESOLVER
                .host_override("request-host.test")
                .is_none()
        );
    }

    #[test]
    fn vanilla_libraries_have_both_bmcl_routes() {
        let source = "https://libraries.minecraft.net/com/example/library/1/library-1.jar";
        let routes = resolve_download_routes_for(
            source,
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::MirrorPreferred,
        );
        assert_eq!(routes.len(), 3);
        assert_eq!(
            routes[0].url,
            "https://bmclapi2.bangbang93.com/maven/com/example/library/1/library-1.jar"
        );
        assert_eq!(
            routes[1].url,
            "https://bmclapi2.bangbang93.com/libraries/com/example/library/1/library-1.jar"
        );
        assert_eq!(routes[2].url, source);
    }

    #[test]
    fn maven_central_libraries_mirror_to_aliyun_with_official_fallback() {
        let source = "https://repo.maven.apache.org/maven2/org/javassist/javassist/3.30.2-GA/javassist-3.30.2-GA.jar";
        let routes = resolve_download_routes_for(
            source,
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::MirrorPreferred,
        );
        assert_eq!(routes.len(), 2);
        assert_eq!(
            routes[0].url,
            "https://maven.aliyun.com/repository/public/org/javassist/javassist/3.30.2-GA/javassist-3.30.2-GA.jar"
        );
        assert!(routes[0].is_mirror);

        assert_eq!(routes[1].url, source);
        assert!(!routes[1].is_mirror);

        let official_only = resolve_download_routes_for(
            source,
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::OfficialOnly,
        );
        assert_eq!(official_only.len(), 1);
        assert_eq!(official_only[0].url, source);
    }

    #[test]
    fn loader_libraries_prefer_mirrors_with_an_official_fallback() {
        let source = "https://libraries.minecraft.net/net/minecraftforge/forge/1.20.1/forge-1.20.1.jar";
        let routes = resolve_download_routes_for(
            source,
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::MirrorPreferred,
        );
        assert_eq!(routes.len(), 3);
        assert_eq!(
            routes[0].url,
            "https://bmclapi2.bangbang93.com/maven/net/minecraftforge/forge/1.20.1/forge-1.20.1.jar"
        );
        assert_eq!(
            routes[1].url,
            "https://bmclapi2.bangbang93.com/libraries/net/minecraftforge/forge/1.20.1/forge-1.20.1.jar"
        );
        assert_eq!(routes[2].url, source);
        assert!(!routes[2].is_mirror);

        let official_only = resolve_download_routes_for(
            source,
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::OfficialOnly,
        );
        assert_eq!(official_only.len(), 1);
        assert_eq!(official_only[0].url, source);

        let official_preferred = resolve_download_routes_for(
            source,
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::OfficialPreferred,
        );
        assert_eq!(official_preferred.len(), 3);
        assert_eq!(official_preferred[0].url, source);
        assert!(official_preferred[1..].iter().all(|route| route.is_mirror));
        assert!(official_preferred.iter().all(|route| {
            resource_family(route, ResourceClass::MinecraftLibrary)
                == ResourceFamily::Loader
        }));
    }

    #[test]
    fn auto_loader_routes_start_official_and_retain_explicit_mirror_preference()
    {
        let _guard = AUTO_SOURCE_TEST_LOCK.lock().unwrap();
        let previous_health = std::mem::take(&mut *ROUTE_HEALTH.lock());
        for source in [
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml",
            "https://maven.minecraftforge.net/net/minecraftforge/forge/maven-metadata.xml",
            "https://maven.fabricmc.net/net/fabricmc/fabric-loader/maven-metadata.xml",
        ] {
            let routes = resolve_download_routes_for(
                source,
                ResourceClass::Loader,
                crate::state::DownloadSourceMode::Auto,
            );
            assert_eq!(routes[0].url, source);
            assert_eq!(routes[0].source, DownloadRouteSource::Official);
            assert!(routes[1..].iter().any(|route| route.is_mirror));
            let mirrors = resolve_download_routes_for(
                source,
                ResourceClass::Loader,
                crate::state::DownloadSourceMode::MirrorPreferred,
            );
            assert!(mirrors[0].is_mirror);
            record_route_failure(&routes[0], ResourceClass::Loader, None);
            let after_failure = resolve_download_routes_for(
                source,
                ResourceClass::Loader,
                crate::state::DownloadSourceMode::Auto,
            );
            assert!(after_failure[0].is_mirror);
        }
        *ROUTE_HEALTH.lock() = previous_health;
    }

    #[test]
    fn maven_central_routes_mirror_to_aliyun_with_official_fallback() {
        for source in [
            "https://repo1.maven.org/maven2/com/example/library/1/library-1.jar?download=1",
            "https://repo.maven.apache.org/maven2/com/example/library/1/library-1.jar?download=1",
        ] {
            let routes = resolve_download_routes_for(
                source,
                ResourceClass::MinecraftLibrary,
                crate::state::DownloadSourceMode::MirrorPreferred,
            );
            assert_eq!(routes.len(), 2);
            assert_eq!(routes[0].source, DownloadRouteSource::Aliyun);
            assert!(routes[0].is_mirror);
            assert!(routes[0]
                .url
                .starts_with("https://maven.aliyun.com/repository/public/com/example/library/1/library-1.jar"));
            assert_eq!(routes[1].url, source);
            assert_eq!(routes[1].source, DownloadRouteSource::Official);
        }

        let unmatched = resolve_download_routes_for(
            "https://repo1.maven.org/repository/com/example/library.jar",
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::MirrorPreferred,
        );
        assert_eq!(unmatched.len(), 1);
        assert_eq!(
            unmatched[0].url,
            "https://repo1.maven.org/repository/com/example/library.jar"
        );

        let prefer_official = resolve_download_routes_for(
            "https://repo1.maven.org/maven2/com/example/library/1/library-1.jar",
            ResourceClass::MinecraftLibrary,
            crate::state::DownloadSourceMode::OfficialOnly,
        );
        assert_eq!(prefer_official.len(), 1);
        assert_eq!(
            prefer_official[0].url,
            "https://repo1.maven.org/maven2/com/example/library/1/library-1.jar"
        );
    }

    #[test]
    fn source_matching_is_origin_safe() {
        assert!(same_origin(
            &Url::parse("https://api.curseforge.com/v1/mods").unwrap(),
            &Url::parse("https://api.curseforge.com/v1/files").unwrap(),
        ));
        assert!(!same_origin(
            &Url::parse("https://api.curseforge.com/v1/mods").unwrap(),
            &Url::parse("https://edge.forgecdn.net/files/1/2/a.jar").unwrap(),
        ));
        assert!(is_sensitive_header("x-api-key"));
        assert!(!header_requires_official_only("x-api-key"));
        assert!(is_sensitive_header("Authorization"));
        assert!(header_requires_official_only("Authorization"));
        assert!(!is_sensitive_header("accept"));
    }

    #[test]
    fn dynamic_ranges_split_the_largest_remaining_tail() {
        let range = DownloadRange::new(0, 0, 10 * 1024 * 1024 - 1);
        let tail = range.split_tail(1).unwrap();
        assert_eq!(range.end(), 6 * 1024 * 1024 - 1);
        assert_eq!(tail.start, 6 * 1024 * 1024);
        assert_eq!(tail.end(), 10 * 1024 * 1024 - 1);
        assert!(tail.remaining() >= 256 * 1024);

        let small = DownloadRange::new(2, 0, 256 * 1024 - 2);
        assert!(small.split_tail(3).is_none());
    }

    #[test]
    fn large_files_start_parallel_ranges_without_consulting_speed_floor() {
        let size = 16 * 1024 * 1024;
        assert_eq!(initial_segment_count(size, 64), 16);
        let ranges = create_initial_ranges(size, 4);
        assert_eq!(ranges.len(), 4);
        assert_eq!(ranges.first().unwrap().start, 0);
        assert_eq!(ranges.last().unwrap().end(), size - 1);
        for pair in ranges.windows(2) {
            assert_eq!(pair[0].end() + 1, pair[1].start);
        }
    }

    #[test]
    fn segmented_concurrency_respects_effective_and_global_limits() {
        assert_eq!(segmented_concurrency_cap(64), MAX_SEGMENT_CONCURRENCY);
        assert_eq!(segmented_concurrency_cap(8), 8);
        assert_eq!(segmented_concurrency_cap(4), 4);
        assert_eq!(segmented_concurrency_cap(1), 1);
        assert_eq!(initial_segment_count(16 * 1024 * 1024, 3), 3);
    }

    #[tokio::test]
    async fn segmented_permits_wait_for_one_slot_instead_of_falling_back() {
        let semaphore =
            FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));
        let held = semaphore
            .0
            .acquire_many(FILE_VALIDATION_CONCURRENCY as u32)
            .await
            .unwrap();
        let route = direct_test_route(
            "https://example.com/file".to_string(),
            DownloadRouteSource::Official,
        );
        let started = Instant::now();
        let (_, permits) = tokio::join!(
            async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                drop(held);
            },
            acquire_initial_segment_permits(&route, &semaphore, 4,),
        );
        let permits = permits.unwrap();
        assert_eq!(permits.len(), 1);
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[test]
    fn segmented_download_caps_are_provider_neutral() {
        let bmclapi = route(
            "https://bmclapi2.bangbang93.com/assets/file".to_string(),
            DownloadRouteSource::Bmclapi,
            true,
            true,
        );
        let official = route(
            "https://resources.download.minecraft.net/file".to_string(),
            DownloadRouteSource::Official,
            false,
            true,
        );

        let modrinth = route(
            "https://cdn.modrinth.com/data/project/version/file.jar".into(),
            DownloadRouteSource::Official,
            false,
            true,
        );
        let size = 8 * 1024 * 1024;
        assert_eq!(route_segmented_concurrency_cap(&bmclapi, 64, size), 32);
        assert_eq!(route_segmented_concurrency_cap(&official, 64, size), 32);
        assert_eq!(route_segmented_concurrency_cap(&modrinth, 64, size), 32);
        let bulk = 100_855_703;
        assert_eq!(route_segmented_concurrency_cap(&modrinth, 64, bulk), 56);
        assert_eq!(route_segmented_concurrency_cap(&modrinth, 96, bulk), 64);
    }

    #[test]
    fn adaptive_ranges_can_expand_and_reserve_capacity_for_small_files() {
        let size = 128 * 1024 * 1024;
        let max = adaptive_range_cap(size, 8, 8, 0, false);
        assert_eq!(max, 8);
        assert!(initial_segment_count(size, 64) < 64);
        assert_eq!(adaptive_range_cap(8 * 1024 * 1024, 8, 8, 0, false), 2);
        assert_eq!(adaptive_range_cap(size, 8, 1, 0, false), 1);
        assert_eq!(adaptive_range_cap(size, 8, 8, 2, false), 2);
        assert_eq!(adaptive_range_cap(size, 8, 8, 4, false), 1);
        let official = route(
            "https://range-fairness.invalid/file".into(),
            DownloadRouteSource::Official,
            false,
            true,
        );
        assert_eq!(route_segmented_concurrency_cap(&official, 4, size), 2);
        assert_eq!(route_segmented_concurrency_cap(&official, 1, size), 1);
        assert_eq!(initial_segment_count(size, 64), 16);
    }

    #[test]
    fn bulk_ranges_can_reach_high_concurrency_without_tiny_file_fanout() {
        let size = 100_855_703;
        assert_eq!(adaptive_range_cap(size, 64, 64, 0, false), 64);
        assert_eq!(adaptive_range_cap(size, 48, 64, 2, false), 2);
        assert_eq!(adaptive_range_cap(size, 48, 1, 0, false), 1);
        assert_eq!(adaptive_range_cap(8 * 1024 * 1024, 48, 64, 0, false), 2);
        assert_eq!(range_growth_target(4, 48, 4, false, 0, 0), 8);
        assert_eq!(range_growth_target(8, 48, 8, false, 0, 0), 16);
        assert_eq!(range_growth_target(16, 48, 16, false, 0, 0), 32);
        assert_eq!(range_growth_target(32, 48, 32, false, 0, 0), 48);
        assert_eq!(range_growth_target(2, 48, 16, true, 0, 0), 16);
        assert_eq!(
            range_growth_target(1, 64, 1, false, 10_000_000, 100_000),
            3
        );
        assert_eq!(range_growth_target(1, 2, 1, false, 10_000_000, 100_000), 2);
        assert_eq!(range_growth_target(16, 64, 16, false, u64::MAX, 1), 64);
    }

    #[test]
    fn tail_floor_scales_with_remaining_data_and_limits() {
        assert_eq!(
            range_connection_floor(100_000_000, 20_000_000, 64, false),
            16
        );
        assert_eq!(
            range_connection_floor(100_000_000, 20_000_000, 2, false),
            2
        );
        assert_eq!(
            range_connection_floor(
                100_000_000,
                MIN_SEGMENT_SIZE * 3,
                64,
                false
            ),
            3
        );
        assert_eq!(range_connection_floor(100_000_000, 1, 64, false), 1);
        assert_eq!(range_connection_floor(9_999_999, 8_000_000, 64, false), 1);
        assert_eq!(range_connection_floor(6_000_000, 4_000_000, 64, true), 15);
        assert_eq!(adaptive_range_cap(6_000_000, 64, 64, 0, true), 22);
        assert_eq!(adaptive_range_cap(6_000_000, 64, 1, 0, true), 1);
        assert_eq!(adaptive_range_cap(6_000_000, 64, 64, 2, true), 2);
    }

    #[test]
    fn tail_floor_recovery_requires_verified_and_splittable_file_data() {
        let mut request = DownloadRequest::new(
            "https://cdn.modrinth.com/tail.jar",
            ResourceClass::Modpack,
        );
        request.integrity.size = Some(6_000_000);
        request.integrity.sha1 = Some("verified-at-completion".into());
        assert!(can_recover_file_ranges(&request, 6_000_000));
        let mut request = request.with_batch_download_policy();
        assert!(can_recover_file_ranges(&request, 6_000_000));
        assert!(can_recover_file_ranges(&request, MIN_SEGMENT_SIZE * 2));
        assert!(!can_recover_file_ranges(&request, MIN_SEGMENT_SIZE * 2 - 1));
        request.integrity.sha1 = None;
        assert!(!can_recover_file_ranges(&request, 6_000_000));
    }

    #[test]
    fn range_recovery_does_not_treat_completed_ranges_as_failed_growth() {
        let baseline = RangeExpansionBaseline {
            speed: 2_712_603,
            completed_ranges: 5,
            previous_target: 3,
        };
        assert_eq!(baseline.gained_throughput(589_316, 8), None);
        assert_eq!(baseline.gained_throughput(589_316, 5), Some(false));
        assert_eq!(baseline.gained_throughput(4_000_000, 5), Some(true));
    }

    #[test]
    fn stalled_growth_can_retry_after_the_startup_burst_collapses() {
        assert!(range_growth_can_retry(5 * 1024 * 1024, 500 * 1024));
        assert!(!range_growth_can_retry(500 * 1024, 490 * 1024));
        assert!(!range_growth_can_retry(0, 0));
        assert!(!range_growth_can_retry(u64::MAX, u64::MAX));
        assert!(range_growth_can_retry(u64::MAX, 1024));
    }

    #[test]
    fn stable_aggregate_throughput_allows_gradual_expansion() {
        let snapshot = SpeedSnapshot {
            aggregate_speed: 2 * 1024 * 1024,
            recent_average: 1900 * 1024,
            speed_floor: 1024 * 1024,
            sample_count: SEGMENT_EXPANSION_SAMPLE_COUNT,
        };
        assert_eq!(
            expansion_block_reason(
                snapshot,
                4,
                8,
                4,
                16 * 1024 * 1024,
                SEGMENT_EXPANSION_INTERVAL,
                SEGMENT_EXPANSION_INTERVAL,
            ),
            None
        );
        assert_eq!(
            expansion_block_reason(
                snapshot,
                4,
                8,
                0,
                16 * 1024 * 1024,
                SEGMENT_EXPANSION_INTERVAL,
                SEGMENT_EXPANSION_INTERVAL,
            ),
            Some("no global permit available")
        );
    }

    #[test]
    fn low_aggregate_throughput_still_allows_more_ranges() {
        let snapshot = SpeedSnapshot {
            aggregate_speed: 64 * 1024,
            recent_average: 80 * 1024,
            speed_floor: 1024 * 1024,
            sample_count: SEGMENT_EXPANSION_SAMPLE_COUNT,
        };
        assert_eq!(
            expansion_block_reason(
                snapshot,
                1,
                8,
                7,
                16 * 1024 * 1024,
                SEGMENT_EXPANSION_INTERVAL,
                SEGMENT_EXPANSION_INTERVAL,
            ),
            None
        );
    }

    #[test]
    fn low_throughput_only_switches_routes_before_fallback_rounds() {
        assert!(allow_low_throughput_route_switch(true, false));
        assert!(!allow_low_throughput_route_switch(true, true));
        assert!(!allow_low_throughput_route_switch(false, false));
    }

    #[test]
    fn slow_http2_downgrades_only_on_the_first_attempt_round() {
        assert!(should_downgrade_slow_http2(reqwest::Version::HTTP_2, false));
        assert!(!should_downgrade_slow_http2(reqwest::Version::HTTP_2, true));
        assert!(!should_downgrade_slow_http2(
            reqwest::Version::HTTP_11,
            false
        ));
    }

    #[test]
    fn reassignable_mirror_uses_shorter_first_byte_timeout() {
        let mirror = route(
            "https://mirror.example/file".to_string(),
            DownloadRouteSource::Bmclapi,
            true,
            true,
        );
        assert_eq!(
            native_first_byte_timeout(&mirror, true),
            REASSIGNABLE_FIRST_BYTE_TIMEOUT
        );
        assert_eq!(
            native_first_byte_timeout(&mirror, false),
            FILE_TRANSFER_FIRST_BYTE_TIMEOUT
        );
    }

    #[test]
    fn official_fallback_excludes_alternate_sources() {
        let alternate = route(
            "https://alternate.example/file".to_string(),
            DownloadRouteSource::Alternate,
            false,
            true,
        );
        let official = route(
            "https://official.example/file".to_string(),
            DownloadRouteSource::Official,
            false,
            true,
        );
        assert_eq!(
            official_fallback_route(&[alternate, official.clone()]),
            Some(official)
        );
    }

    #[test]
    fn segmented_download_starts_at_four_mebibytes() {
        assert!(!should_use_segmented_download(
            SEGMENTED_DOWNLOAD_THRESHOLD - 1,
            0,
        ));
        assert!(should_use_segmented_download(
            SEGMENTED_DOWNLOAD_THRESHOLD,
            0,
        ));
        assert!(should_use_segmented_download(
            24 * 1024 * 1024,
            16 * 1024 * 1024
        ));
        assert!(!should_use_segmented_download(
            24 * 1024 * 1024,
            23 * 1024 * 1024
        ));
    }

    #[test]
    fn route_probe_requires_a_twenty_five_percent_improvement() {
        assert!(!probe_is_meaningfully_faster(1_249_999, 1_000_000));
        assert!(probe_is_meaningfully_faster(1_250_000, 1_000_000));
        assert!(!probe_is_meaningfully_faster(u64::MAX, u64::MAX));
        assert_eq!(
            measured_bytes_per_second(
                ROUTE_PROBE_BYTES,
                Duration::from_millis(250),
            ),
            1024 * 1024,
        );
    }

    #[test]
    fn effective_authority_drives_health_without_removing_fallback_urls() {
        let alias = route(
            "https://effective-authority-alias.invalid/file.jar".to_string(),
            DownloadRouteSource::Mcim,
            true,
            true,
        );
        let direct = route(
            "https://effective-authority-target.invalid/file.jar".to_string(),
            DownloadRouteSource::Official,
            false,
            true,
        );
        remember_effective_route_authority(&alias, &direct.url);

        assert_eq!(
            route_health_key(&alias, ResourceClass::Other),
            route_health_key(&direct, ResourceClass::Other),
        );
        assert!(routes_share_effective_authority(&alias, &direct));

        let mut direct_without_proxy = direct.clone();
        direct_without_proxy.proxy = ProxyPolicy::Direct;
        let mut routes = vec![alias, direct, direct_without_proxy.clone()];
        deduplicate_download_routes(&mut routes);

        assert_eq!(routes.len(), 3);
        assert_eq!(routes[2], direct_without_proxy);
    }

    #[test]
    fn route_deduplication_preserves_distinct_paths_on_one_authority() {
        let first = route(
            "https://mirror.example/maven/library.jar".to_string(),
            DownloadRouteSource::Bmclapi,
            true,
            true,
        );
        let mut second = first.clone();
        second.url = "https://mirror.example/libraries/library.jar".to_string();
        let mut routes = vec![first.clone(), second.clone(), first];

        deduplicate_download_routes(&mut routes);

        assert_eq!(
            routes,
            vec![
                route(
                    "https://mirror.example/maven/library.jar".to_string(),
                    DownloadRouteSource::Bmclapi,
                    true,
                    true,
                ),
                second,
            ]
        );
    }

    #[test]
    fn effective_authority_memory_is_scoped_to_the_exact_route_url() {
        let redirected = route(
            "https://route-scope.example/cache-miss.jar".to_string(),
            DownloadRouteSource::Bmclapi,
            true,
            true,
        );
        let direct = route(
            "https://route-scope.example/cache-hit.jar".to_string(),
            DownloadRouteSource::Bmclapi,
            true,
            true,
        );
        remember_effective_route_authority(
            &redirected,
            "https://official.example/cache-miss.jar",
        );

        assert_eq!(
            effective_route_authority(&redirected).as_deref(),
            Some("official.example:443"),
        );
        assert_eq!(
            effective_route_authority(&direct).as_deref(),
            Some("route-scope.example:443"),
        );
    }

    #[test]
    fn h2_route_selection_skips_an_open_breaker_when_possible() {
        let blocked = route(
            "https://blocked-h2.example/file".to_string(),
            DownloadRouteSource::Alternate,
            true,
            true,
        );
        let healthy = route(
            "https://healthy-h2.example/file".to_string(),
            DownloadRouteSource::Official,
            false,
            true,
        );
        for _ in 0..3 {
            crate::util::download::native_breaker::record_failure(&blocked);
        }

        assert_eq!(
            first_h2_route(&[blocked.clone(), healthy.clone()]),
            Some(healthy)
        );
        assert_eq!(
            first_h2_route(std::slice::from_ref(&blocked)),
            Some(blocked.clone())
        );
        crate::util::download::native_breaker::record_success(&blocked);
    }

    #[tokio::test]
    async fn route_probe_selects_a_faster_distinct_authority() {
        let data = Arc::new(vec![7_u8; ROUTE_PROBE_BYTES as usize * 2]);
        let (url, requests, _, server) =
            spawn_range_server(data.clone(), RangeServerBehavior::default())
                .await;
        let current = route(
            "https://route-probe-current.invalid/file.jar".to_string(),
            DownloadRouteSource::Official,
            false,
            true,
        );
        let mut candidate =
            route(url, DownloadRouteSource::Alternate, false, true);
        candidate.proxy = ProxyPolicy::Direct;
        let candidates = vec![candidate.clone()];
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let semaphore = FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY));

        let probe = probe_faster_route(
            &current,
            &candidates,
            64 * 1024,
            data.len() as u64,
            None,
            None,
            None,
            &semaphore,
            &client,
            &client,
            ResourceClass::Other,
            0,
        )
        .await
        .expect("the local range route should be measurably faster");

        assert_eq!(probe.route, candidate);
        assert!(probe.bytes_per_second >= 80 * 1024);
        assert_eq!(requests.load(Ordering::Relaxed), 1);

        let alias = route(
            "https://route-probe-alias.invalid/file.jar".to_string(),
            DownloadRouteSource::Mcim,
            true,
            true,
        );
        remember_effective_route_authority(&alias, &candidate.url);
        assert!(
            probe_faster_route(
                &alias,
                &candidates,
                1,
                data.len() as u64,
                None,
                None,
                None,
                &semaphore,
                &client,
                &client,
                ResourceClass::Other,
                0,
            )
            .await
            .is_none()
        );
        assert_eq!(requests.load(Ordering::Relaxed), 1);
        server.abort();
    }

    #[derive(Clone, Copy)]
    enum H2FailureFixture {
        Slow,
        Reset,
        ReceiveTimeout,
    }

    async fn check_h2_fallback(
        resume: bool,
        with_alternate: bool,
        failure: H2FailureFixture,
    ) {
        let data = Arc::new(vec![7_u8; 256 * 1024]);
        let (url, requests, normal_requests, http1_server) =
            spawn_range_server(
                data.clone(),
                RangeServerBehavior {
                    fail_first_range: with_alternate,
                    ..Default::default()
                },
            )
            .await;
        let mut primary =
            route(url.clone(), DownloadRouteSource::Official, false, true);
        primary.proxy = ProxyPolicy::Direct;
        let mut routes = vec![primary.clone()];
        let mut alternate_server = None;
        let mut alternate_requests = None;
        if with_alternate {
            let (url, requests, _, server) =
                spawn_range_server(data.clone(), Default::default()).await;
            let mut alternate =
                route(url, DownloadRouteSource::Alternate, false, true);
            alternate.proxy = ProxyPolicy::Direct;
            routes.push(alternate);
            alternate_server = Some(server);
            alternate_requests = Some(requests);
        }
        let (client_io, server_io) = tokio::io::duplex(1024 * 1024);
        let h2_data = data.clone();
        let finish_healthy = Arc::new(tokio::sync::Notify::new());
        let finish_server = finish_healthy.clone();
        let h2_server = tokio::spawn(async move {
            let mut server = h2::server::handshake(server_io).await.unwrap();
            let (_, mut healthy) = server.accept().await.unwrap().unwrap();
            let mut healthy_body = healthy
                .send_response(http::Response::new(()), false)
                .unwrap();
            healthy_body
                .send_data(bytes::Bytes::from_static(&[3; 1024]), false)
                .unwrap();
            let healthy_sender = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = finish_server.notified() => break,
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {
                            healthy_body.send_data(bytes::Bytes::from_static(&[3; 1024]), false).unwrap();
                        }
                    }
                }
                healthy_body
                    .send_data(bytes::Bytes::from_static(b"done"), true)
                    .unwrap();
            });
            let (_, mut respond) = server.accept().await.unwrap().unwrap();
            let response = http::Response::builder()
                .header("content-length", h2_data.len())
                .body(())
                .unwrap();
            let mut body = respond.send_response(response, false).unwrap();
            let sender = tokio::spawn(async move {
                match failure {
                    H2FailureFixture::Reset => {
                        body.send_reset(h2::Reason::CANCEL);
                        return;
                    }
                    H2FailureFixture::ReceiveTimeout => {
                        std::future::pending::<()>().await;
                        drop(body);
                        return;
                    }
                    H2FailureFixture::Slow => {}
                }
                for chunk in h2_data.chunks(1024) {
                    if body
                        .send_data(bytes::Bytes::copy_from_slice(chunk), false)
                        .is_err()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
            while server.accept().await.is_some() {}
            sender.abort();
            healthy_sender.abort();
        });
        let (sender, driver) = h2::client::handshake(client_io).await.unwrap();
        let connection = super::super::download::h2_pool::SharedH2Connection::cache_for_test(&primary, sender).await;
        let driver = super::super::download::h2_pool::spawn_connection_driver(
            &connection,
            driver,
            Duration::from_secs(60),
        );
        let healthy_activity = connection.track_stream();
        let mut scheduler = super::super::download::authority_scheduler::AuthorityScheduler::default();
        scheduler.add_connection(connection.clone());
        let mut healthy_body = connection
            .open(
                http::Request::builder()
                    .uri("https://test.invalid/healthy")
                    .body(())
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_body();
        let healthy_download = tokio::spawn(async move {
            let _activity = healthy_activity;
            let mut received = Vec::new();
            while let Some(chunk) = healthy_body.data().await {
                let chunk = chunk.unwrap();
                healthy_body
                    .flow_control()
                    .release_capacity(chunk.len())
                    .unwrap();
                received.extend_from_slice(&chunk);
            }
            received
        });
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("fallback.bin");
        let part = suffixed_path(&destination, ".part");
        let integrity = if resume {
            Integrity::sha1(sha1_smol::Sha1::from(&data[..]).hexdigest())
                .with_size(data.len() as u64)
        } else {
            Integrity::default().with_size(data.len() as u64)
        };
        let request = DownloadRequest::new(&url, ResourceClass::Other)
            .with_integrity(integrity);
        let semaphore =
            FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));
        let mut policy = super::super::download::native::h2_policy(
            &primary,
            request.integrity.size,
        )
        .await
        .unwrap();
        if !matches!(failure, H2FailureFixture::Slow) {
            policy.abort_if_slow = false;
        }
        assert!(matches!(
            try_h2_download(
                &request,
                primary.clone(),
                policy,
                &destination,
                &part,
                &semaphore
            )
            .await
            .unwrap(),
            H2AttemptResult::Fallback { .. }
        ));
        assert!(!connection.is_retiring());
        assert!(
            !connection.is_dead(),
            "healthy stream must keep its driver alive"
        );
        assert!(
            super::super::download::h2_pool::has_live_connection(&primary)
                .await
        );
        assert!(scheduler.select_connection().is_some());
        connection.release_stream_reservation();
        scheduler.prune();
        assert!(scheduler.select_connection().is_some());
        connection.release_stream_reservation();
        finish_healthy.notify_one();
        let healthy_bytes =
            tokio::time::timeout(Duration::from_secs(3), healthy_download)
                .await
                .unwrap()
                .unwrap();
        assert!(healthy_bytes.len() > 4);
        assert!(
            healthy_bytes[..healthy_bytes.len() - 4]
                .iter()
                .all(|byte| *byte == 3)
        );
        assert!(healthy_bytes.ends_with(b"done"));
        connection.retire();
        tokio::time::timeout(Duration::from_secs(3), driver)
            .await
            .unwrap()
            .unwrap();
        assert!(connection.is_dead());
        assert!(!authority_uses_http1_fallback_for(
            &url_authority(&url).unwrap(),
            primary.proxy
        ));
        if resume {
            let len = tokio::fs::metadata(&part).await.unwrap().len();
            assert!(len > 0 && len < data.len() as u64);
            assert_eq!(
                tokio::fs::read(&part).await.unwrap(),
                data[..len as usize]
            );
        } else {
            assert!(!part.exists());
        }
        let result = run_native_download_attempts(
            request,
            &destination,
            &semaphore,
            None,
            routes,
            part,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result.size, data.len() as u64);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        assert!(requests.load(Ordering::Relaxed) > 0);
        if resume && !with_alternate {
            assert_eq!(normal_requests.load(Ordering::Relaxed), 0);
        }
        if let Some(requests) = alternate_requests {
            assert!(requests.load(Ordering::Relaxed) > 0);
        }
        if let Some(server) = alternate_server {
            server.abort();
        }
        h2_server.abort();
        http1_server.abort();
    }

    #[tokio::test]
    async fn slow_h2_fallback_restarts_single_source_over_http1() {
        check_h2_fallback(false, false, H2FailureFixture::Slow).await;
    }

    #[tokio::test]
    async fn slow_h2_fallback_resumes_and_verifies_partial_over_http1() {
        check_h2_fallback(true, false, H2FailureFixture::Slow).await;
    }

    #[tokio::test]
    async fn slow_h2_fallback_can_recover_using_alternate_cdn() {
        check_h2_fallback(true, true, H2FailureFixture::Slow).await;
    }

    #[tokio::test]
    async fn reset_h2_stream_does_not_interrupt_healthy_sibling() {
        check_h2_fallback(false, false, H2FailureFixture::Reset).await;
    }

    #[tokio::test]
    async fn timed_out_h2_stream_does_not_interrupt_healthy_sibling() {
        check_h2_fallback(false, false, H2FailureFixture::ReceiveTimeout).await;
    }

    #[tokio::test]
    async fn recovery_probe_prefers_measured_http1_without_recording_protocol_failure()
     {
        let data = Arc::new(vec![9_u8; 512 * 1024]);
        let (target_url, target_requests, _, target_server) =
            spawn_range_server(data.clone(), Default::default()).await;
        let (url, redirect_requests, redirect_server) =
            spawn_redirect_server(target_url.clone(), Duration::ZERO).await;
        let mut primary =
            route(url.clone(), DownloadRouteSource::Official, false, true);
        primary.proxy = ProxyPolicy::Direct;
        let request = DownloadRequest::new(&url, ResourceClass::Other)
            .with_integrity(Integrity::default().with_size(data.len() as u64));
        let semaphore =
            FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));
        let selected = probe_recovery_route(
            &request,
            &primary,
            &[],
            1024,
            data.len() as u64,
            0,
            true,
            None,
            &semaphore,
        )
        .await
        .unwrap();
        let original = url_authority(&url).unwrap();
        let target = url_authority(&target_url).unwrap();
        assert_ne!(original, target);
        assert_eq!(selected.effective_authority, target);
        assert!(authority_uses_http1_fallback_for(&original, primary.proxy));
        assert!(authority_uses_http1_fallback_for(&target, primary.proxy));
        for authority in [&original, &target] {
            assert!(!H2_FALLBACK_AUTHORITIES.lock().contains_key(
                &super::super::download::proxy_context::authority_key(
                    authority,
                    primary.proxy
                )
            ));
        }
        assert_eq!(redirect_requests.load(Ordering::Relaxed), 1);
        assert_eq!(target_requests.load(Ordering::Relaxed), 1);
        redirect_server.abort();
        target_server.abort();
    }

    #[tokio::test]
    async fn recovery_probe_compares_http1_and_alternate_cdn() {
        for http1_is_faster in [false, true] {
            let data = Arc::new(vec![9_u8; 512 * 1024]);
            let (url, primary_requests, _, primary_server) =
                spawn_range_server(
                    data.clone(),
                    RangeServerBehavior {
                        slow_body: !http1_is_faster,
                        ..Default::default()
                    },
                )
                .await;
            let (alternate_url, alternate_requests, _, alternate_server) =
                spawn_range_server(
                    data.clone(),
                    RangeServerBehavior {
                        slow_body: http1_is_faster,
                        ..Default::default()
                    },
                )
                .await;
            let mut primary =
                route(url.clone(), DownloadRouteSource::Official, false, true);
            primary.proxy = ProxyPolicy::Direct;
            let mut alternate = route(
                alternate_url,
                DownloadRouteSource::Alternate,
                false,
                true,
            );
            alternate.proxy = ProxyPolicy::Direct;
            let request = DownloadRequest::new(&url, ResourceClass::Other)
                .with_integrity(
                    Integrity::sha1(
                        sha1_smol::Sha1::from(&data[..]).hexdigest(),
                    )
                    .with_size(data.len() as u64),
                );
            let semaphore =
                FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));
            let selected = probe_recovery_route(
                &request,
                &primary,
                std::slice::from_ref(&alternate),
                1024,
                data.len() as u64,
                0,
                true,
                None,
                &semaphore,
            )
            .await
            .unwrap();
            assert_eq!(
                selected.route,
                if http1_is_faster {
                    primary.clone()
                } else {
                    alternate
                }
            );
            assert_eq!(primary_requests.load(Ordering::Relaxed), 1);
            assert_eq!(alternate_requests.load(Ordering::Relaxed), 1);
            assert_eq!(
                authority_uses_http1_fallback_for(
                    &url_authority(&url).unwrap(),
                    primary.proxy
                ),
                http1_is_faster
            );
            primary_server.abort();
            alternate_server.abort();
        }
    }

    #[test]
    fn redirect_hops_rebuild_the_same_range_header() {
        let original = byte_range_header_value(Some(1024), Some(2047));
        let redirected = byte_range_header_value(Some(1024), Some(2047));
        assert_eq!(original.as_deref(), Some("bytes=1024-2047"));
        assert_eq!(redirected, original);
    }

    #[tokio::test]
    async fn verifies_streaming_integrity_algorithms() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"axolotl download").unwrap();
        let integrity = Integrity {
            size: Some(16),
            sha1: Some("90e438ead880c77ea2d7e726b5aa74e6d21a805f".to_string()),
            sha512: Some("2dcd3e0a9f198e9ef892a28ed6534dd154be2bd13531c961c0852aa6f1e24f633d2cd8288d3cf13c6a482a87c822b74a4901a2aa64292f4a371c5ebfea392c1b".to_string()),
            sha256: Some("120561bc60d59ebe2a08fc229ff2b1eb06b20c4211d21a17c15dd80790f48672".to_string()),
            md5: Some("30018bb52add8c6dbc5d4149c1325df0".to_string()),
            content: ContentValidation::None,
        };
        assert_eq!(verify_file(file.path(), &integrity).await.unwrap(), 16);
    }

    #[tokio::test]
    async fn segmented_download_uses_parallel_validated_ranges() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        RANGE_SPLITTING_SUPPORTED.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2 + 1024 * 1024) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, normal_requests, server) = spawn_range_server(
            data.clone(),
            RangeServerBehavior {
                slow_body: true,
                ..Default::default()
            },
        )
        .await;
        let route = DownloadRoute {
            url: url.clone(),
            source: DownloadRouteSource::Alternate,
            is_mirror: false,
            allow_sensitive_headers: false,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        };
        let request = DownloadRequest::new(&url, ResourceClass::Other)
            .with_integrity(Integrity::sha1(hash).with_size(size as u64));
        let directory = tempfile::tempdir().unwrap();
        let part_path = directory.path().join("fixture.part");
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let semaphore = FetchSemaphore(Semaphore::new(8));
        let progress_samples = Arc::new(Mutex::new(Vec::new()));
        let samples = Arc::clone(&progress_samples);
        let mut progress: Box<FetchProgressFn<'_>> =
            Box::new(move |bytes, _| {
                samples.lock().push(bytes);
                Box::pin(async { Ok(()) })
            });
        let outcome = try_segmented_download(
            SegmentedDownloadContext::new(
                &request,
                &route,
                &[],
                size as u64,
                &part_path,
                &semaphore,
                None,
                &client,
                &client,
                1,
                1,
                false,
            ),
            Some(&mut progress),
        )
        .await;
        match outcome {
            SegmentedDownloadOutcome::Success(result) => {
                assert_eq!(result.size, size as u64);
            }
            _ => panic!("segmented fixture download did not succeed"),
        }
        let samples = progress_samples.lock();
        assert!(samples.first().is_some_and(|bytes| *bytes <= 512 * 1024));
        assert!(samples.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(samples.iter().all(|bytes| *bytes <= size as u64));
        drop(samples);
        assert!(requests.load(Ordering::Relaxed) >= 2);
        assert_eq!(normal_requests.load(Ordering::Relaxed), 0);
        assert!(RANGE_SPLITTING_SUPPORTED.lock().contains(
            &super::super::download::proxy_context::authority_key(
                &range_splitting_authority(&route).unwrap(),
                route.proxy
            )
        ));
        assert_eq!(
            verify_file(&part_path, &request.integrity).await.unwrap(),
            size as u64
        );
        for index in 0..MAX_SEGMENT_CONCURRENCY {
            assert!(
                !segment_path(&part_path, index).exists(),
                "direct range output must not create segment files"
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn stalled_tail_range_is_hedged_and_temp_files_are_cleaned() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, _, server) = spawn_range_server(
            data,
            RangeServerBehavior {
                stall_first_range: true,
                ..Default::default()
            },
        )
        .await;
        let route = DownloadRoute {
            url: url.clone(),
            source: DownloadRouteSource::Alternate,
            is_mirror: false,
            allow_sensitive_headers: false,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        };
        let request = DownloadRequest::new(&url, ResourceClass::Other)
            .with_integrity(Integrity::sha1(hash).with_size(size as u64));
        let directory = tempfile::tempdir().unwrap();
        let part_path = directory.path().join("hedged.part");
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let semaphore = FetchSemaphore(Semaphore::new(8));
        let started = Instant::now();
        let outcome = try_segmented_download(
            SegmentedDownloadContext::new(
                &request,
                &route,
                &[],
                size as u64,
                &part_path,
                &semaphore,
                None,
                &client,
                &client,
                1,
                1,
                false,
            ),
            None,
        )
        .await;
        assert!(matches!(outcome, SegmentedDownloadOutcome::Success(_)));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(requests.load(Ordering::Relaxed) > 2);
        assert_eq!(
            verify_file(&part_path, &request.integrity).await.unwrap(),
            size as u64
        );
        for range_index in 0..MAX_SEGMENT_CONCURRENCY {
            for candidate_index in 0..2 {
                assert!(
                    !tail_candidate_path(
                        &part_path,
                        range_index,
                        candidate_index,
                    )
                    .exists()
                );
            }
        }
        server.abort();
    }

    #[tokio::test]
    async fn mirror_ranges_start_concurrently_and_retries_reuse_redirect() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (target_url, range_requests, normal_requests, range_server) =
            spawn_range_server(
                data,
                RangeServerBehavior {
                    fail_first_range: true,
                    ..Default::default()
                },
            )
            .await;
        let (redirect_url, redirect_requests, redirect_server) =
            spawn_redirect_server(target_url, Duration::from_millis(50)).await;
        let route = DownloadRoute {
            url: redirect_url.clone(),
            source: DownloadRouteSource::Mcim,
            is_mirror: true,
            allow_sensitive_headers: false,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        };
        let request =
            DownloadRequest::new(&redirect_url, ResourceClass::Modrinth)
                .with_integrity(Integrity::sha1(hash).with_size(size as u64));
        let directory = tempfile::tempdir().unwrap();
        let part_path = directory.path().join("redirect.part");
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let outcome = try_segmented_download(
            SegmentedDownloadContext::new(
                &request,
                &route,
                &[],
                size as u64,
                &part_path,
                &FetchSemaphore(Semaphore::new(8)),
                None,
                &client,
                &client,
                1,
                1,
                false,
            ),
            None,
        )
        .await;

        assert!(matches!(outcome, SegmentedDownloadOutcome::Success(_)));
        assert_eq!(redirect_requests.load(Ordering::Relaxed), 2);
        assert!(range_requests.load(Ordering::Relaxed) > 2);
        assert_eq!(normal_requests.load(Ordering::Relaxed), 0);
        redirect_server.abort();
        range_server.abort();
    }

    #[tokio::test]
    async fn temporary_range_failure_resumes_without_disabling_segments() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, _, server) = spawn_range_server(
            data,
            RangeServerBehavior {
                fail_first_range: true,
                ..Default::default()
            },
        )
        .await;
        let route = DownloadRoute {
            url: url.clone(),
            source: DownloadRouteSource::Alternate,
            is_mirror: false,
            allow_sensitive_headers: false,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        };
        let request = DownloadRequest::new(&url, ResourceClass::Other)
            .with_integrity(Integrity::sha1(hash).with_size(size as u64));
        let directory = tempfile::tempdir().unwrap();
        let part_path = directory.path().join("retry.part");
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let outcome = try_segmented_download(
            SegmentedDownloadContext::new(
                &request,
                &route,
                &[],
                size as u64,
                &part_path,
                &FetchSemaphore(Semaphore::new(8)),
                None,
                &client,
                &client,
                1,
                1,
                false,
            ),
            None,
        )
        .await;

        assert!(matches!(outcome, SegmentedDownloadOutcome::Success(_)));
        assert!(requests.load(Ordering::Relaxed) > 2);
        assert_eq!(
            verify_file(&part_path, &request.integrity).await.unwrap(),
            size as u64
        );
        server.abort();
    }

    #[tokio::test]
    async fn invalid_content_range_disables_range_splitting_after_repeats() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let data = Arc::new(vec![7_u8; 1024 * 1024]);
        let (url, _, _, server) = spawn_range_server(
            data.clone(),
            RangeServerBehavior {
                wrong_content_range: true,
                ..Default::default()
            },
        )
        .await;
        let route = DownloadRoute {
            url: url.clone(),
            source: DownloadRouteSource::Alternate,
            is_mirror: false,
            allow_sensitive_headers: false,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        };
        let directory = tempfile::tempdir().unwrap();
        let part_path = directory.path().join("fixture.part");
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let semaphore =
            FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));
        let (progress, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let permit =
            acquire_native_connection(&route, &semaphore).await.unwrap();
        let speed = DownloadSpeedTracker::default();
        let validator = Mutex::new(None);
        let output = crate::util::download::range_output::RangeOutput::create(
            &part_path,
            data.len() as u64,
        )
        .await
        .unwrap();
        let result = download_segment(
            &route,
            DownloadRange::new(0, 0, data.len() as u64 - 1),
            data.len() as u64,
            None,
            None,
            None,
            &part_path,
            &output,
            permit,
            &client,
            &client,
            progress,
            &speed,
            &validator,
            None,
            &semaphore,
            &AtomicUsize::new(0),
            &AtomicBool::new(false),
            &DownloadRequest::new(&route.url, ResourceClass::Other),
        )
        .await;
        assert!(matches!(
            result,
            Err(SegmentDownloadError::Protocol("invalid Content-Range"))
        ));
        disable_range_splitting(&route);
        assert!(
            range_splitting_allowed(&route),
            "a single protocol failure should not disable range splitting"
        );
        disable_range_splitting(&route);
        assert!(
            !range_splitting_allowed(&route),
            "repeated protocol failures should disable range splitting"
        );
        server.abort();
    }

    #[tokio::test]
    async fn mod_validator_changes_do_not_disable_the_next_pack_ranges() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        let size = 12 * 1024 * 1024;
        let data = Arc::new(vec![23_u8; size]);
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, normal_requests, server) = spawn_range_server(
            data.clone(),
            RangeServerBehavior {
                vary_mod_validators: true,
                ..Default::default()
            },
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let semaphore = FetchSemaphore(Semaphore::new(32));

        for file in ["first.jar", "second.jar", "next.mrpack"] {
            let mut file_url = url::Url::parse(&url).unwrap();
            file_url.set_path(file);
            let file_url = file_url.to_string();
            let destination = directory.path().join(file);
            let part = suffixed_path(&destination, ".part");
            let _disk =
                super::super::download::local_resources::set_test_disk_kind(
                    &part,
                    sysinfo::DiskKind::SSD,
                );
            let integrity = if file.ends_with(".mrpack") {
                Integrity::sha1(&hash).with_size(size as u64)
            } else {
                Integrity::default().with_size(size as u64)
            };
            let request = DownloadRequest::new(&file_url, ResourceClass::Other)
                .with_integrity(integrity);
            let before = requests.load(Ordering::Relaxed);
            let result =
                download_benchmark_fixture(request, &destination, &semaphore)
                    .await
                    .unwrap();
            assert_eq!(result.size, size as u64);
            assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
            let file_route =
                route(file_url, DownloadRouteSource::Official, false, true);
            assert!(
                range_splitting_allowed(&file_route),
                "object version changes must not blacklist the authority"
            );
            if file.ends_with(".mrpack") {
                assert!(
                    requests.load(Ordering::Relaxed) - before >= 4,
                    "the next pack must still use independent ranges"
                );
                assert_eq!(normal_requests.load(Ordering::Relaxed), 2);
            }
        }

        assert_eq!(semaphore.0.available_permits(), 32);
        server.abort();
    }

    #[tokio::test]
    async fn verified_files_keep_32_ranges_on_unknown_disks_when_validators_differ()
     {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        let size = 16 * 1024 * 1024;
        let data = Arc::new(vec![31_u8; size]);
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, normal_requests, server) = spawn_range_server(
            data.clone(),
            RangeServerBehavior {
                vary_range_validators: true,
                ..Default::default()
            },
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let semaphore = FetchSemaphore(Semaphore::new(64));

        for (resource, valid_hash, known_size) in [
            (ResourceClass::Modpack, true, true),
            (ResourceClass::MinecraftLibrary, true, true),
            (ResourceClass::Java, true, false),
            (ResourceClass::Modrinth, true, true),
            (ResourceClass::CurseForge, false, true),
        ] {
            let destination = directory
                .path()
                .join(format!("file-{resource:?}-{valid_hash}"));
            let part = suffixed_path(&destination, ".part");
            let _disk =
                super::super::download::local_resources::set_test_disk_kind(
                    &part,
                    sysinfo::DiskKind::Unknown(0),
                );
            let mut integrity = Integrity::sha1(if valid_hash {
                &hash
            } else {
                "0000000000000000000000000000000000000000"
            });
            if known_size {
                integrity.size = Some(size as u64);
            }
            let request =
                DownloadRequest::new(&url, resource).with_integrity(integrity);
            let before = requests.load(Ordering::Relaxed);
            let result =
                download_benchmark_fixture(request, &destination, &semaphore)
                    .await;
            assert!(
                requests.load(Ordering::Relaxed) - before >= 32,
                "{resource:?}, known_size={known_size}: fewer than 32 requests"
            );
            if valid_hash {
                assert_eq!(
                    normal_requests.load(Ordering::Relaxed),
                    usize::from(!known_size),
                    "{resource:?}"
                );
            }
            if valid_hash {
                assert_eq!(result.unwrap().size, size as u64);
                assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
            } else {
                assert!(
                    result.is_err(),
                    "metadata tolerance must never publish content with the wrong hash"
                );
                assert!(!destination.exists());
            }
            assert_eq!(semaphore.0.available_permits(), 64);
            normal_requests.store(0, Ordering::Relaxed);
        }
        server.abort();
    }

    #[tokio::test]
    async fn ignored_ranges_fall_back_to_one_full_file_write() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let (url, requests, normal_requests, server) = spawn_range_server(
            data.clone(),
            RangeServerBehavior {
                ignore_range: true,
                ..Default::default()
            },
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("ignored-range.bin");
        let result = download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other)
                .with_integrity(Integrity::default().with_size(size as u64)),
            &destination,
            &FetchSemaphore(Semaphore::new(8)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, size as u64);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        assert!(requests.load(Ordering::Relaxed) >= 2);
        assert_eq!(normal_requests.load(Ordering::Relaxed), 1);
        for index in 0..MAX_SEGMENT_CONCURRENCY {
            assert!(
                !segment_path(&suffixed_path(&destination, ".part"), index)
                    .exists()
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn canceling_segmented_download_releases_permits_and_temp_files() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 4) as usize;
        let data = Arc::new(vec![13_u8; size]);
        let (url, _, _, server) = spawn_range_server(
            data,
            RangeServerBehavior {
                slow_body: true,
                ..Default::default()
            },
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("canceled-segments.bin");
        let destination_for_task = destination.clone();
        let semaphore = Arc::new(FetchSemaphore(Semaphore::new(8)));
        let semaphore_for_task = Arc::clone(&semaphore);
        let task = tokio::spawn(async move {
            download_to_path(
                DownloadRequest::new(url, ResourceClass::Other).with_integrity(
                    Integrity::default().with_size(size as u64),
                ),
                destination_for_task,
                &semaphore_for_task,
                None,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        task.abort();
        let _ = task.await;
        tokio::time::sleep(Duration::from_millis(25)).await;

        assert_eq!(semaphore.0.available_permits(), 8);
        let part_path = suffixed_path(&destination, ".part");
        assert!(!part_path.exists());
        for index in 0..MAX_SEGMENT_CONCURRENCY {
            assert!(!segment_path(&part_path, index).exists());
        }
        server.abort();
    }

    #[tokio::test]
    async fn valid_existing_destination_is_reused_without_network_request() {
        let data = Arc::new(b"already complete".to_vec());
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, _, server) =
            spawn_range_server(data.clone(), RangeServerBehavior::default())
                .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("existing.bin");
        tokio::fs::write(&destination, &data[..]).await.unwrap();

        let result = download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other).with_integrity(
                Integrity::sha1(hash).with_size(data.len() as u64),
            ),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.attempts, 0);
        assert_eq!(requests.load(Ordering::Relaxed), 0);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        server.abort();
    }

    #[tokio::test]
    async fn invalid_existing_destination_is_redownloaded() {
        let data = Arc::new(b"correct content".to_vec());
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, _, server) =
            spawn_range_server(data.clone(), RangeServerBehavior::default())
                .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("stale.bin");
        tokio::fs::write(&destination, b"wrong content")
            .await
            .unwrap();

        download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other).with_integrity(
                Integrity::sha1(hash).with_size(data.len() as u64),
            ),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(requests.load(Ordering::Relaxed), 1);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        server.abort();
    }

    #[tokio::test]
    async fn unknown_size_and_small_files_use_one_connection() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        for (expected_size, integrity) in [
            (None, Integrity::default()),
            (Some(1024_u64), Integrity::default().with_size(1024)),
        ] {
            let data = Arc::new(vec![11_u8; 1024]);
            let (url, requests, normal_requests, server) = spawn_range_server(
                data.clone(),
                RangeServerBehavior::default(),
            )
            .await;
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("single.bin");
            let result = download_to_path(
                DownloadRequest::new(&url, ResourceClass::Other)
                    .with_integrity(integrity),
                &destination,
                &FetchSemaphore(Semaphore::new(8)),
                None,
            )
            .await
            .unwrap();
            assert_eq!(result.size, expected_size.unwrap_or(1024));
            assert_eq!(requests.load(Ordering::Relaxed), 1);
            assert_eq!(normal_requests.load(Ordering::Relaxed), 1);
            server.abort();
        }
    }

    #[tokio::test]
    async fn single_connection_downloads_resume_from_partial_files() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = 512 * 1024_usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, normal_requests, server) =
            spawn_range_server(data.clone(), RangeServerBehavior::default())
                .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("resumed.bin");
        let part_path = suffixed_path(&destination, ".part");
        tokio::fs::write(&part_path, &data[..size / 2])
            .await
            .unwrap();
        let result = download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other)
                .with_integrity(Integrity::sha1(hash).with_size(size as u64)),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, size as u64);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        assert_eq!(
            requests.load(Ordering::Relaxed),
            1,
            "the resumed download should finish with one range request"
        );
        assert_eq!(
            normal_requests.load(Ordering::Relaxed),
            0,
            "the resumed download should not restart from the beginning"
        );
        assert!(!part_path.exists());
        server.abort();
    }

    #[tokio::test]
    async fn stale_partial_files_with_wrong_content_self_heal() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = 512 * 1024_usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, _, _, server) =
            spawn_range_server(data.clone(), RangeServerBehavior::default())
                .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("stale.bin");
        let part_path = suffixed_path(&destination, ".part");
        tokio::fs::write(&part_path, vec![0xAB_u8; size / 2])
            .await
            .unwrap();
        let result = download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other)
                .with_integrity(Integrity::sha1(hash).with_size(size as u64)),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, size as u64);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        server.abort();
    }

    #[tokio::test]
    async fn switching_sources_does_not_resume_another_sources_partial_file() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = 512 * 1024_usize;
        let expected = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let stale = Arc::new(vec![0xAB_u8; size - 4096]);
        let hash = sha1_smol::Sha1::from(&expected[..]).hexdigest();
        let (stale_url, stale_requests, stale_normal_requests, stale_server) =
            spawn_range_server(stale, RangeServerBehavior::default()).await;
        let (
            official_url,
            official_requests,
            official_normal_requests,
            official_server,
        ) = spawn_range_server(
            expected.clone(),
            RangeServerBehavior::default(),
        )
        .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("source-isolated.bin");

        let result = download_to_path(
            DownloadRequest::new(&stale_url, ResourceClass::Other)
                .with_candidate_urls([official_url])
                .with_integrity(Integrity::sha1(hash).with_size(size as u64)),
            &destination,
            &FetchSemaphore(Semaphore::new(ROUTE_PROBE_CONCURRENCY)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.source, DownloadRouteSource::Alternate);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *expected);
        assert_eq!(stale_requests.load(Ordering::Relaxed), 1);
        assert_eq!(stale_normal_requests.load(Ordering::Relaxed), 1);
        assert_eq!(official_requests.load(Ordering::Relaxed), 1);
        assert_eq!(
            official_normal_requests.load(Ordering::Relaxed),
            1,
            "the fallback source must restart instead of resuming mirror bytes",
        );
        stale_server.abort();
        official_server.abort();
    }

    #[tokio::test]
    async fn segmented_fallback_keeps_partial_files_for_resume() {
        let _guard = RANGE_SPLITTING_TEST_LOCK.lock().await;
        RANGE_SPLITTING_PROTOCOL_FAILURES.lock().clear();
        let size = (SEGMENTED_DOWNLOAD_THRESHOLD * 2) as usize;
        let data = Arc::new(
            (0..size)
                .map(|index| (index % 251) as u8)
                .collect::<Vec<_>>(),
        );
        let hash = sha1_smol::Sha1::from(&data[..]).hexdigest();
        let (url, requests, normal_requests, server) =
            spawn_range_server(data.clone(), RangeServerBehavior::default())
                .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("fallback-resume.bin");
        let part_path = suffixed_path(&destination, ".part");
        tokio::fs::write(&part_path, &data[..size / 4])
            .await
            .unwrap();
        // A single global permit forces the segmented attempt to fall back
        // before transferring anything; the preserved partial data must
        // survive that fallback and drive a resumed single connection.
        let result = download_to_path(
            DownloadRequest::new(&url, ResourceClass::Other)
                .with_integrity(Integrity::sha1(hash).with_size(size as u64)),
            &destination,
            &FetchSemaphore(Semaphore::new(1)),
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.size, size as u64);
        assert_eq!(tokio::fs::read(&destination).await.unwrap(), *data);
        assert_eq!(
            requests.load(Ordering::Relaxed),
            1,
            "the fallback should finish with one resumed range request"
        );
        assert_eq!(
            normal_requests.load(Ordering::Relaxed),
            0,
            "the preserved partial data should not be discarded"
        );
        server.abort();
    }

    #[tokio::test]
    async fn nonempty_json_fetch_rejects_empty_collections() {
        let clients = DownloadClients::build(
            &crate::util::proxy::ProxyConfig {
                mode: crate::util::proxy::ProxyMode::None,
                ..Default::default()
            },
            false,
            false,
        )
        .unwrap();
        super::super::download::proxy_context::with_snapshot(clients, async {
            let semaphore =
                FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));

            let (url, requests, server) =
                spawn_json_server("[]".to_string()).await;
            let result = fetch_json_nonempty::<Vec<serde_json::Value>>(
                Method::GET,
                &url,
                None,
                None,
                None,
                &semaphore,
            )
            .await;
            assert!(
                result.is_err(),
                "an empty JSON array must be rejected as invalid data"
            );
            assert_eq!(requests.load(Ordering::Relaxed), 1);
            server.abort();

            let (url, requests, server) =
                spawn_json_server("[1,2,3]".to_string()).await;
            let result = fetch_json_nonempty::<Vec<serde_json::Value>>(
                Method::GET,
                &url,
                None,
                None,
                None,
                &semaphore,
            )
            .await;
            let values =
                result.expect("a non-empty JSON array should be accepted");
            assert_eq!(values.len(), 3);
            assert_eq!(requests.load(Ordering::Relaxed), 1);
            server.abort();
        })
        .await;
    }

    #[tokio::test]
    async fn regular_json_fetch_still_accepts_empty_collections() {
        let clients = DownloadClients::build(
            &crate::util::proxy::ProxyConfig {
                mode: crate::util::proxy::ProxyMode::None,
                ..Default::default()
            },
            false,
            false,
        )
        .unwrap();
        super::super::download::proxy_context::with_snapshot(clients, async {
            let semaphore =
                FetchSemaphore(Semaphore::new(FILE_VALIDATION_CONCURRENCY));

            let (url, requests, server) =
                spawn_json_server("[]".to_string()).await;
            let result = fetch_json::<Vec<serde_json::Value>>(
                Method::GET,
                &url,
                None,
                None,
                None,
                &semaphore,
            )
            .await;
            let values = result
                .expect("regular JSON fetches keep accepting empty arrays");
            assert!(values.is_empty());
            assert_eq!(requests.load(Ordering::Relaxed), 1);
            server.abort();
        })
        .await;
    }

    #[test]
    fn h2_fallback_authority_marking() {
        let _guard = H2_FALLBACK_TEST_LOCK.lock();
        record_authority_h2_failure("example.com:443");
        assert!(authority_uses_http1_fallback("example.com:443"));
        assert!(!authority_uses_http1_fallback("other.com:443"));
        H2_FALLBACK_AUTHORITIES.lock().clear();
    }
}
