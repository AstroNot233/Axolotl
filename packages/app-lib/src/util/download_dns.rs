use hickory_resolver::proto::{
    op::{Message, MessageType, Query, ResponseCode},
    rr::{Name as DnsName, RData, RecordType},
};
use parking_lot::Mutex;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DEFAULT_DOH_SERVER: &str = "https://doh.pub/dns-query";

/// `lookup_host` does not expose the authoritative record TTL. Keep entries
/// long enough to retain the connection-reuse benefit, but short enough that
/// a changed CDN, VPN, or network is not pinned until the application exits.
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const CONNECTION_FAILURES_BEFORE_REFRESH: u8 = 2;
const DNS_LOOKUP_TIMEOUT: Duration = Duration::from_secs(6);
const DOH_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const SECOND_FAMILY_GRACE: Duration = Duration::from_millis(250);
const FAILURE_CACHE_TTL: Duration = Duration::from_secs(2);
const MAX_DNS_MESSAGE_BYTES: usize = 65535;

#[derive(Clone)]
struct CachedFailure {
    kind: std::io::ErrorKind,
    message: String,
    failed_at: Instant,
}

#[derive(Clone)]
struct CachedAddresses {
    addresses: Vec<IpAddr>,
    resolved_at: Instant,
    consecutive_connection_failures: u8,
}

impl CachedAddresses {
    fn is_fresh(&self) -> bool {
        self.resolved_at.elapsed() < CACHE_TTL
    }
}

#[derive(Clone)]
pub struct DownloadDnsResolver {
    reliability: Arc<Mutex<HashMap<IpAddr, f64>>>,
    last_resolved: Arc<Mutex<HashMap<String, CachedAddresses>>>,
    last_failed: Arc<Mutex<HashMap<String, CachedFailure>>>,
    /// Locks only a single hostname's lookup. The map is held just long
    /// enough to obtain the per-host lock, never while DNS is awaited.
    resolving_hosts: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    host_overrides: Arc<Mutex<HashMap<String, String>>>,
    doh_enabled: bool,
    doh_server: Arc<str>,
    doh_client: reqwest::Client,
    #[cfg(test)]
    test_addresses: Arc<Mutex<HashMap<String, Vec<SocketAddr>>>>,
    #[cfg(test)]
    test_lookup_delays: Arc<Mutex<HashMap<String, Duration>>>,
    #[cfg(test)]
    test_lookup_errors: Arc<Mutex<HashMap<String, std::io::ErrorKind>>>,
    #[cfg(test)]
    test_lookup_counts: Arc<Mutex<HashMap<String, usize>>>,
}

impl Default for DownloadDnsResolver {
    fn default() -> Self {
        Self {
            reliability: Arc::default(),
            last_resolved: Arc::default(),
            last_failed: Arc::default(),
            resolving_hosts: Arc::default(),
            host_overrides: Arc::default(),
            doh_enabled: false,
            doh_server: Arc::from(DEFAULT_DOH_SERVER),
            doh_client: reqwest::Client::builder()
                .no_proxy()
                .timeout(DOH_REQUEST_TIMEOUT)
                .connect_timeout(Duration::from_secs(3))
                .build()
                .expect("DNS bootstrap client configuration should be valid"),
            #[cfg(test)]
            test_addresses: Arc::default(),
            #[cfg(test)]
            test_lookup_delays: Arc::default(),
            #[cfg(test)]
            test_lookup_errors: Arc::default(),
            #[cfg(test)]
            test_lookup_counts: Arc::default(),
        }
    }
}

impl DownloadDnsResolver {
    pub fn with_doh(
        enabled: bool,
        server: impl Into<String>,
    ) -> crate::Result<Self> {
        Self::with_doh_and_proxy(enabled, server, None)
    }

    pub fn with_doh_and_proxy(
        enabled: bool,
        server: impl Into<String>,
        proxy: Option<&crate::util::proxy::ProxyConfig>,
    ) -> crate::Result<Self> {
        let server = server.into().trim().to_string();
        if enabled {
            let parsed = reqwest::Url::parse(&server).map_err(|error| {
                crate::ErrorKind::InputError(format!(
                    "DoH server URL is invalid: {error}"
                ))
            })?;
            if parsed.scheme() != "https" || parsed.host_str().is_none() {
                return Err(crate::ErrorKind::InputError(
                    "DoH server must be an HTTPS URL".to_string(),
                )
                .into());
            }
        }
        let builder = match proxy {
            Some(proxy) => proxy.apply(reqwest::Client::builder())?,
            None => reqwest::Client::builder().no_proxy(),
        };
        let doh_client = builder
            .timeout(DOH_REQUEST_TIMEOUT)
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(DOH_REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(crate::Error::from)?;
        Ok(Self {
            doh_enabled: enabled,
            doh_server: Arc::from(server),
            doh_client,
            ..Self::default()
        })
    }

    pub fn doh_enabled(&self) -> bool {
        self.doh_enabled
    }

    pub fn doh_server(&self) -> &str {
        &self.doh_server
    }
    /// Resolves `host` through `resolver_host` while preserving the original
    /// URL host for HTTP Host headers and TLS SNI.
    #[allow(dead_code)]
    pub fn set_host_override(
        &self,
        host: &str,
        resolver_host: &str,
    ) -> Result<(), &'static str> {
        let host = normalize_host(host)?;
        let resolver_host = normalize_host(resolver_host)?;
        let mut overrides = self.host_overrides.lock();
        if host == resolver_host {
            overrides.remove(&host);
        } else {
            overrides.insert(host.clone(), resolver_host);
        }
        drop(overrides);
        self.last_resolved.lock().remove(&host);
        self.last_failed.lock().remove(&host);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn clear_host_override(&self, host: &str) -> Result<(), &'static str> {
        let host = normalize_host(host)?;
        self.host_overrides.lock().remove(&host);
        self.last_resolved.lock().remove(&host);
        self.last_failed.lock().remove(&host);
        Ok(())
    }

    pub fn host_override(&self, host: &str) -> Option<String> {
        let host = normalize_host(host).ok()?;
        self.host_overrides.lock().get(&host).cloned()
    }

    fn resolution_host(&self, host: &str) -> String {
        self.host_override(host).unwrap_or_else(|| host.to_string())
    }
    pub fn record_result(&self, address: IpAddr, result: f64) {
        let mut reliability = self.reliability.lock();
        reliability
            .entry(address)
            .and_modify(|value| *value = *value * 0.5 + result * 0.5)
            .or_insert(result * 0.5);
    }

    pub fn record_host_success(&self, host: &str, address: IpAddr) {
        let mut cached = self.last_resolved.lock();
        if let Some(entry) = cached
            .get_mut(host)
            .filter(|entry| entry.addresses.contains(&address))
        {
            entry.consecutive_connection_failures = 0;
            drop(cached);
            self.record_result(address, 0.5);
        }
    }

    pub fn resolved_addresses(&self, host: &str) -> Vec<IpAddr> {
        self.last_resolved
            .lock()
            .get(host)
            .filter(|entry| entry.is_fresh())
            .map(|entry| &entry.addresses)
            .cloned()
            .unwrap_or_default()
    }

    /// Marks a failed connection attempt for `host`. The second consecutive
    /// failure expires its cache entry, so the next request or prewarm does a
    /// fresh lookup. A single transient failure keeps the hot cache intact.
    /// Returns whether this call expired the entry.
    pub fn record_connection_failure(&self, host: &str) -> bool {
        let mut cached = self.last_resolved.lock();
        let Some(entry) = cached.get_mut(host) else {
            return false;
        };
        entry.consecutive_connection_failures =
            entry.consecutive_connection_failures.saturating_add(1);
        if entry.consecutive_connection_failures
            < CONNECTION_FAILURES_BEFORE_REFRESH
        {
            return false;
        }
        entry.resolved_at = Instant::now() - CACHE_TTL;
        true
    }

    fn cache_addresses(&self, host: String, addresses: Vec<SocketAddr>) {
        self.last_resolved.lock().insert(
            host,
            CachedAddresses {
                addresses: addresses
                    .iter()
                    .map(|address| address.ip())
                    .collect(),
                resolved_at: Instant::now(),
                consecutive_connection_failures: 0,
            },
        );
    }

    fn resolving_lock(&self, host: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.resolving_hosts
            .lock()
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    async fn lookup_addresses(
        &self,
        resolution_host: &str,
    ) -> std::io::Result<Vec<SocketAddr>> {
        #[cfg(test)]
        {
            *self
                .test_lookup_counts
                .lock()
                .entry(resolution_host.to_string())
                .or_default() += 1;
            let delay =
                self.test_lookup_delays.lock().get(resolution_host).copied();
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            if let Some(kind) =
                self.test_lookup_errors.lock().get(resolution_host).copied()
            {
                return Err(std::io::Error::new(
                    kind,
                    "test DNS lookup failed",
                ));
            }
            if let Some(addresses) =
                self.test_addresses.lock().get(resolution_host).cloned()
            {
                return Ok(addresses);
            }
        }
        if self.doh_enabled {
            return self.lookup_doh(resolution_host).await;
        }
        tokio::net::lookup_host((resolution_host, 0))
            .await
            .map(|addresses| addresses.collect())
    }

    async fn lookup_doh(&self, host: &str) -> std::io::Result<Vec<SocketAddr>> {
        resolve_families(
            self.lookup_doh_family(host, RecordType::A),
            self.lookup_doh_family(host, RecordType::AAAA),
        )
        .await
    }

    async fn lookup_doh_family(
        &self,
        host: &str,
        record_type: RecordType,
    ) -> std::io::Result<Vec<SocketAddr>> {
        let query = doh_query(host, record_type)?;
        let body = query.to_vec().map_err(std::io::Error::other)?;
        let mut response = self
            .doh_client
            .post(self.doh_server.as_ref())
            .header(reqwest::header::CONTENT_TYPE, "application/dns-message")
            .header(reqwest::header::ACCEPT, "application/dns-message")
            .body(body)
            .send()
            .await
            .map_err(std::io::Error::other)?
            .error_for_status()
            .map_err(std::io::Error::other)?;
        let mut bytes = Vec::new();
        while let Some(chunk) =
            response.chunk().await.map_err(std::io::Error::other)?
        {
            if bytes.len() + chunk.len() > MAX_DNS_MESSAGE_BYTES {
                return Err(std::io::Error::other("DoH response is too large"));
            }
            bytes.extend_from_slice(&chunk);
        }
        parse_doh_response(&bytes, &query)
    }

    fn cached_failure(&self, host: &str) -> Option<std::io::Error> {
        self.last_failed
            .lock()
            .get(host)
            .filter(|failure| failure.failed_at.elapsed() < FAILURE_CACHE_TTL)
            .map(|failure| {
                std::io::Error::new(failure.kind, failure.message.clone())
            })
    }

    async fn refresh(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        tokio::time::timeout(DNS_LOOKUP_TIMEOUT, self.refresh_inner(host))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("DNS lookup for {host} timed out"),
                )
            })?
    }

    async fn refresh_inner(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        let host = normalize_host(host).map_err(std::io::Error::other)?;
        let cached = self.resolved_addresses(&host);
        if !cached.is_empty() {
            return Ok(cached);
        }
        if let Some(error) = self.cached_failure(&host) {
            return Err(error);
        }
        let host_lock = self.resolving_lock(&host);
        let _guard = host_lock.lock().await;
        let cached = self.resolved_addresses(&host);
        if !cached.is_empty() {
            return Ok(cached);
        }
        if let Some(error) = self.cached_failure(&host) {
            return Err(error);
        }
        let resolution_host = self.resolution_host(&host);
        let lookup = tokio::time::timeout(
            DOH_REQUEST_TIMEOUT + SECOND_FAMILY_GRACE,
            self.lookup_addresses(&resolution_host),
        )
        .await
        .unwrap_or_else(|_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("DNS lookup for {host} timed out"),
            ))
        })
        .and_then(|addresses| {
            if addresses.is_empty() {
                Err(std::io::Error::other(
                    "DNS response contained no addresses",
                ))
            } else {
                Ok(addresses)
            }
        });
        let mut addresses = match lookup {
            Ok(addresses) => addresses,
            Err(error) => {
                self.last_failed.lock().insert(
                    host,
                    CachedFailure {
                        kind: error.kind(),
                        message: error.to_string(),
                        failed_at: Instant::now(),
                    },
                );
                return Err(error);
            }
        };
        addresses = self.order_addresses(&host, addresses);
        let resolved = addresses.iter().map(|address| address.ip()).collect();
        self.cache_addresses(host, addresses);
        Ok(resolved)
    }

    /// Resolves a host ahead of the first request so batch downloads can
    /// share a single ordered address list. Failed lookups are briefly cached
    /// so a batch shares the failure instead of repeating the same request.
    pub async fn pre_resolve(&self, host: &str) {
        let _ = self.refresh(host).await;
    }

    pub(crate) async fn resolve_host(
        &self,
        host: &str,
    ) -> std::io::Result<Vec<IpAddr>> {
        self.refresh(host).await
    }

    #[cfg(test)]
    fn set_test_addresses(&self, host: &str, addresses: Vec<SocketAddr>) {
        self.test_addresses
            .lock()
            .insert(host.to_string(), addresses);
    }

    #[cfg(test)]
    fn set_test_lookup_delay(&self, host: &str, delay: Duration) {
        self.test_lookup_delays
            .lock()
            .insert(host.to_string(), delay);
    }

    #[cfg(test)]
    fn expire_cache(&self, host: &str) {
        if let Some(entry) = self.last_resolved.lock().get_mut(host) {
            entry.resolved_at = Instant::now() - CACHE_TTL;
        }
    }

    fn score(&self, address: IpAddr) -> f64 {
        self.reliability
            .lock()
            .get(&address)
            .copied()
            .unwrap_or_default()
    }

    fn order_addresses(
        &self,
        host: &str,
        mut addresses: Vec<SocketAddr>,
    ) -> Vec<SocketAddr> {
        addresses.sort_unstable_by_key(|address| address.ip());
        addresses.dedup_by_key(|address| address.ip());

        let best_v4 = addresses
            .iter()
            .filter(|address| address.is_ipv4())
            .map(|address| self.score(address.ip()))
            .max_by(f64::total_cmp);
        let mut best_v6 = addresses
            .iter()
            .filter(|address| address.is_ipv6())
            .map(|address| self.score(address.ip()))
            .max_by(f64::total_cmp);
        if host == "api.modrinth.com" {
            best_v6 = best_v6.map(|score| score - 0.1);
        }
        addresses.sort_unstable_by(|left, right| {
            let preferred_v4 =
                best_v4.unwrap_or_default() >= best_v6.unwrap_or_default();
            let left_family = left.is_ipv4() == preferred_v4;
            let right_family = right.is_ipv4() == preferred_v4;
            right_family.cmp(&left_family).then_with(|| {
                self.score(right.ip()).total_cmp(&self.score(left.ip()))
            })
        });
        addresses
    }
}

fn doh_query(host: &str, record_type: RecordType) -> std::io::Result<Message> {
    let mut name = DnsName::from_ascii(host).map_err(std::io::Error::other)?;
    name.set_fqdn(true);
    let mut message = Message::new();
    message
        .set_id(rand::random())
        .set_recursion_desired(true)
        .add_query(Query::query(name, record_type));
    Ok(message)
}

fn parse_doh_response(
    bytes: &[u8],
    query: &Message,
) -> std::io::Result<Vec<SocketAddr>> {
    let response = Message::from_vec(bytes).map_err(std::io::Error::other)?;
    if response.id() != query.id()
        || response.message_type() != MessageType::Response
        || response.queries() != query.queries()
        || response.truncated()
        || response.response_code() != ResponseCode::NoError
    {
        return Err(std::io::Error::other(
            "Invalid or unsuccessful DoH DNS response",
        ));
    }
    let question = query
        .query()
        .ok_or_else(|| std::io::Error::other("Missing DNS question"))?;
    let mut names = std::collections::HashSet::from([question.name().clone()]);
    for _ in 0..response.answers().len() {
        let mut changed = false;
        for answer in response.answers() {
            if names.contains(answer.name())
                && let RData::CNAME(alias) = answer.data()
            {
                changed |= names.insert(alias.0.clone());
            }
        }
        if !changed {
            break;
        }
    }
    let addresses = response
        .answers()
        .iter()
        .filter(|record| {
            names.contains(record.name())
                && record.record_type() == question.query_type()
                && record.dns_class() == question.query_class()
        })
        .filter_map(|record| match record.data() {
            RData::A(address) => {
                Some(SocketAddr::new(IpAddr::V4(address.0), 0))
            }
            RData::AAAA(address) => {
                Some(SocketAddr::new(IpAddr::V6(address.0), 0))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(std::io::Error::other(
            "DoH response contained no addresses",
        ));
    }
    Ok(addresses)
}

async fn resolve_families(
    ipv4: impl std::future::Future<Output = std::io::Result<Vec<SocketAddr>>>,
    ipv6: impl std::future::Future<Output = std::io::Result<Vec<SocketAddr>>>,
) -> std::io::Result<Vec<SocketAddr>> {
    tokio::pin!(ipv4, ipv6);
    let (first, second) = tokio::select! {
        first = &mut ipv4 => {
            let wait = if first.is_ok() { SECOND_FAMILY_GRACE } else { DOH_REQUEST_TIMEOUT };
            (first, tokio::time::timeout(wait, &mut ipv6).await.ok())
        }
        first = &mut ipv6 => {
            let wait = if first.is_ok() { SECOND_FAMILY_GRACE } else { DOH_REQUEST_TIMEOUT };
            (first, tokio::time::timeout(wait, &mut ipv4).await.ok())
        }
    };
    match (first, second) {
        (Ok(mut addresses), Some(Ok(other))) => {
            addresses.extend(other);
            Ok(addresses)
        }
        (Ok(addresses), _) | (Err(_), Some(Ok(addresses))) => Ok(addresses),
        (Err(error), _) => Err(error),
    }
}

fn normalize_host(host: &str) -> Result<String, &'static str> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty()
        || host.contains(['/', ':', '@', '[', ']'])
        || host.split('.').any(str::is_empty)
    {
        return Err("DNS host override must be a hostname without a port");
    }
    Ok(host)
}

impl Resolve for DownloadDnsResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let resolver = self.clone();
        Box::pin(async move {
            let addresses = resolver
                .refresh(&host)
                .await?
                .into_iter()
                .map(|address| SocketAddr::new(address, 0))
                .collect::<Vec<_>>();
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn doh_configuration_requires_https_and_keeps_the_default() {
        assert_eq!(DEFAULT_DOH_SERVER, "https://doh.pub/dns-query");
        assert!(
            DownloadDnsResolver::with_doh(true, "http://dns.example/query")
                .is_err()
        );
        let resolver =
            DownloadDnsResolver::with_doh(true, DEFAULT_DOH_SERVER).unwrap();
        assert!(resolver.doh_enabled());
        assert_eq!(resolver.doh_server(), DEFAULT_DOH_SERVER);
    }

    #[test]
    fn disabled_doh_accepts_an_invalid_draft_endpoint() {
        let resolver = DownloadDnsResolver::with_doh(false, "").unwrap();
        assert!(!resolver.doh_enabled());
        assert!(DownloadDnsResolver::with_doh(true, "").is_err());
    }

    #[test]
    fn wire_queries_and_answers_round_trip_and_validate_the_question() {
        use hickory_resolver::proto::rr::{
            Record,
            rdata::{A, AAAA, CNAME},
        };
        for record_type in [RecordType::A, RecordType::AAAA] {
            let query = doh_query("cdn.example", record_type).unwrap();
            let decoded = Message::from_vec(&query.to_vec().unwrap()).unwrap();
            assert_eq!(decoded.queries(), query.queries());
            assert!(decoded.recursion_desired());
            let alias = DnsName::from_ascii("edge.example").unwrap();
            let ip = if record_type == RecordType::A {
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))
            } else {
                IpAddr::V6(Ipv6Addr::LOCALHOST)
            };
            let data = match ip {
                IpAddr::V4(ip) => RData::A(A(ip)),
                IpAddr::V6(ip) => RData::AAAA(AAAA(ip)),
            };
            let mut response = query.clone();
            response
                .set_message_type(MessageType::Response)
                .add_answer(Record::from_rdata(
                    query.query().unwrap().name().clone(),
                    60,
                    RData::CNAME(CNAME(alias.clone())),
                ))
                .add_answer(Record::from_rdata(alias, 60, data));
            assert_eq!(
                parse_doh_response(&response.to_vec().unwrap(), &query)
                    .unwrap(),
                vec![SocketAddr::new(ip, 0)]
            );
            response.set_id(query.id().wrapping_add(1));
            assert!(
                parse_doh_response(&response.to_vec().unwrap(), &query)
                    .is_err()
            );
        }
        assert!(
            parse_doh_response(
                b"{\"Answer\":[]}",
                &doh_query("cdn.example", RecordType::A).unwrap()
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn usable_family_survives_a_failed_or_stalled_other_family() {
        let address = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 0));
        let failed = async { Err(std::io::Error::other("AAAA failed")) };
        assert_eq!(
            resolve_families(async { Ok(vec![address]) }, failed)
                .await
                .unwrap(),
            vec![address]
        );
        let addresses = tokio::time::timeout(
            Duration::from_secs(1),
            resolve_families(
                async { Ok(vec![address]) },
                std::future::pending(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(addresses, vec![address]);
    }

    #[tokio::test]
    async fn failed_lookup_is_shared_across_a_batch_and_expires() {
        let resolver = DownloadDnsResolver::default();
        let host = "failed-batch.test";
        resolver
            .test_lookup_errors
            .lock()
            .insert(host.into(), std::io::ErrorKind::TimedOut);
        resolver.set_test_lookup_delay(host, Duration::from_millis(20));
        let results =
            futures::future::join_all((0..45).map(|_| resolver.refresh(host)))
                .await;
        assert!(results.iter().all(Result::is_err));
        assert_eq!(resolver.test_lookup_counts.lock().get(host), Some(&1));
        resolver.last_failed.lock().get_mut(host).unwrap().failed_at -=
            FAILURE_CACHE_TTL;
        resolver.test_lookup_errors.lock().remove(host);
        resolver.set_test_addresses(
            host,
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        );
        assert!(resolver.refresh(host).await.is_ok());
        assert_eq!(resolver.test_lookup_counts.lock().get(host), Some(&2));
    }

    #[tokio::test]
    async fn lookup_and_host_lock_waits_have_deadlines() {
        let resolver = DownloadDnsResolver::default();
        let host = "deadline.test";
        resolver.set_test_addresses(
            host,
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        );
        resolver.set_test_lookup_delay(host, DNS_LOOKUP_TIMEOUT * 2);
        let error = resolver.refresh(host).await.unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        let lock = resolver.resolving_lock("locked.test");
        let _guard = lock.lock().await;
        assert_eq!(
            resolver.refresh("locked.test").await.unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    async fn spawn_ipv4_server() -> (u16, tokio::task::JoinHandle<()>) {
        let listener =
            tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                )
                .await
                .unwrap();
        });
        (port, handle)
    }

    async fn request_with_resolver(
        resolver: DownloadDnsResolver,
        host: &str,
        port: u16,
    ) -> String {
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .dns_resolver(Arc::new(resolver))
            .build()
            .unwrap()
            .get(format!("http://{host}:{port}/"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    }

    #[test]
    fn returns_both_protocol_families_in_preferred_order() {
        let resolver = DownloadDnsResolver::default();
        let ipv4 = SocketAddr::from((Ipv4Addr::new(203, 0, 113, 10), 0));
        let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 0));
        resolver.record_result(ipv6.ip(), -0.7);

        assert_eq!(
            resolver.order_addresses("api.modrinth.com", vec![ipv6, ipv4]),
            vec![ipv4, ipv6]
        );
    }

    #[test]
    fn selects_the_most_reliable_address_within_a_family() {
        let resolver = DownloadDnsResolver::default();
        let slower = SocketAddr::from((Ipv4Addr::new(203, 0, 113, 10), 0));
        let faster = SocketAddr::from((Ipv4Addr::new(203, 0, 113, 11), 0));
        resolver.record_result(faster.ip(), 0.5);

        assert_eq!(
            resolver.order_addresses("cdn.example.com", vec![slower, faster]),
            vec![faster, slower]
        );
    }

    #[test]
    fn only_records_the_address_that_completed_the_request() {
        let resolver = DownloadDnsResolver::default();
        let failed = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10));
        let succeeded = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11));
        resolver.cache_addresses(
            "cdn.example.com".to_string(),
            vec![SocketAddr::new(failed, 0), SocketAddr::new(succeeded, 0)],
        );

        resolver.record_host_success("cdn.example.com", succeeded);

        assert_eq!(resolver.score(failed), 0.0);
        assert!(resolver.score(succeeded) > 0.0);
    }

    #[test]
    fn host_success_does_not_refresh_an_expired_dns_entry() {
        let resolver = DownloadDnsResolver::default();
        let address = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 11));
        resolver.cache_addresses(
            "expired-success.test".to_string(),
            vec![SocketAddr::new(address, 0)],
        );
        resolver.expire_cache("expired-success.test");

        resolver.record_host_success("expired-success.test", address);

        assert!(
            resolver
                .resolved_addresses("expired-success.test")
                .is_empty()
        );
    }

    #[test]
    fn late_success_does_not_undo_failure_expiration() {
        let resolver = DownloadDnsResolver::default();
        let address = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 12));
        resolver.cache_addresses(
            "late-success.test".to_string(),
            vec![SocketAddr::new(address, 0)],
        );
        assert!(!resolver.record_connection_failure("late-success.test"));
        assert!(resolver.record_connection_failure("late-success.test"));

        resolver.record_host_success("late-success.test", address);

        assert!(resolver.resolved_addresses("late-success.test").is_empty());
    }

    #[tokio::test]
    async fn one_request_falls_back_when_the_first_ip_refuses_connection() {
        let resolver = DownloadDnsResolver::default();
        let refused = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 2), 0));
        let available = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        resolver.set_test_addresses("multi.test", vec![refused, available]);
        resolver.record_result(refused.ip(), 1.0);
        let (port, server) = spawn_ipv4_server().await;

        let body = request_with_resolver(resolver, "multi.test", port).await;

        assert_eq!(body, "ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn one_request_falls_back_from_ipv6_to_ipv4() {
        let resolver = DownloadDnsResolver::default();
        let unavailable_v6 = SocketAddr::from((Ipv6Addr::LOCALHOST, 0));
        let available_v4 = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        resolver.set_test_addresses(
            "dual-stack.test",
            vec![unavailable_v6, available_v4],
        );
        resolver.record_result(unavailable_v6.ip(), 1.0);
        let (port, server) = spawn_ipv4_server().await;

        let body =
            request_with_resolver(resolver, "dual-stack.test", port).await;

        assert_eq!(body, "ok");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn expired_cache_refreshes_to_the_current_addresses() {
        let resolver = DownloadDnsResolver::default();
        let old = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 2), 0));
        let current = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        resolver.set_test_addresses("ttl-refresh.test", vec![old]);
        resolver.pre_resolve("ttl-refresh.test").await;
        resolver.set_test_addresses("ttl-refresh.test", vec![current]);

        assert_eq!(
            resolver.resolved_addresses("ttl-refresh.test"),
            vec![old.ip()]
        );
        resolver.expire_cache("ttl-refresh.test");
        resolver.pre_resolve("ttl-refresh.test").await;

        assert_eq!(
            resolver.resolved_addresses("ttl-refresh.test"),
            vec![current.ip()]
        );
    }

    #[tokio::test]
    async fn repeated_connection_failures_refresh_the_cached_address() {
        let resolver = DownloadDnsResolver::default();
        let old = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 2), 0));
        let current = SocketAddr::from((Ipv4Addr::LOCALHOST, 0));
        resolver.set_test_addresses("failure-refresh.test", vec![old]);
        resolver.pre_resolve("failure-refresh.test").await;
        resolver.set_test_addresses("failure-refresh.test", vec![current]);

        assert!(!resolver.record_connection_failure("failure-refresh.test"));
        assert!(resolver.record_connection_failure("failure-refresh.test"));
        assert!(
            resolver
                .resolved_addresses("failure-refresh.test")
                .is_empty()
        );
        resolver.pre_resolve("failure-refresh.test").await;

        assert_eq!(
            resolver.resolved_addresses("failure-refresh.test"),
            vec![current.ip()]
        );
        let (port, server) = spawn_ipv4_server().await;
        assert_eq!(
            request_with_resolver(resolver, "failure-refresh.test", port).await,
            "ok"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn pre_resolving_one_host_does_not_block_another_host() {
        let resolver = DownloadDnsResolver::default();
        resolver.set_test_addresses(
            "slow-resolution.test",
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        );
        resolver.set_test_lookup_delay(
            "slow-resolution.test",
            Duration::from_millis(100),
        );
        resolver.set_test_addresses(
            "fast-resolution.test",
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        );

        let slow_resolver = resolver.clone();
        let slow = tokio::spawn(async move {
            slow_resolver.pre_resolve("slow-resolution.test").await;
        });
        tokio::task::yield_now().await;
        tokio::time::timeout(
            Duration::from_millis(50),
            resolver.pre_resolve("fast-resolution.test"),
        )
        .await
        .expect("an unrelated DNS lookup must not wait for the slow host");

        assert!(
            !resolver
                .resolved_addresses("fast-resolution.test")
                .is_empty()
        );
        slow.await.unwrap();
    }

    #[tokio::test]
    async fn host_override_uses_the_target_hosts_addresses() {
        let resolver = DownloadDnsResolver::default();
        let (port, server) = spawn_ipv4_server().await;
        resolver.set_test_addresses(
            "resolver-target.test",
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        );
        resolver
            .set_host_override("REQUEST-HOST.TEST.", "resolver-target.test")
            .unwrap();

        let body =
            request_with_resolver(resolver.clone(), "request-host.test", port)
                .await;

        assert_eq!(body, "ok");
        assert_eq!(
            resolver.host_override("request-host.test").as_deref(),
            Some("resolver-target.test"),
        );
        assert!(!resolver.resolved_addresses("request-host.test").is_empty());
        server.await.unwrap();
    }

    #[test]
    fn clearing_override_removes_the_cached_request_host_addresses() {
        let resolver = DownloadDnsResolver::default();
        resolver
            .set_host_override("request-host.test", "resolver-target.test")
            .unwrap();
        resolver.cache_addresses(
            "request-host.test".to_string(),
            vec![SocketAddr::from((Ipv4Addr::LOCALHOST, 0))],
        );

        resolver.clear_host_override("request-host.test").unwrap();

        assert!(resolver.host_override("request-host.test").is_none());
        assert!(resolver.resolved_addresses("request-host.test").is_empty());
        assert!(normalize_host("https://resolver-target.test").is_err());
        assert!(normalize_host("resolver-target.test:443").is_err());
    }

    #[test]
    fn default_resolver_does_not_override_tianpao() {
        assert!(
            DownloadDnsResolver::default()
                .host_override("mod.tianpao.top")
                .is_none()
        );
    }

    #[test]
    fn default_resolver_does_not_override_legacy_modrinth_cdn() {
        assert!(
            DownloadDnsResolver::default()
                .host_override("cdn.modrinth.com")
                .is_none()
        );
    }
}
