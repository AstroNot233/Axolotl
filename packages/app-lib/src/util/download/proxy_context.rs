//! Identity and resolver selection for download network configurations.

use crate::util::download_dns::DownloadDnsResolver;
use crate::util::fetch::ProxyPolicy;
use crate::util::proxy::ProxyConfig;
use sha2::{Digest, Sha256};
use std::sync::Arc;

tokio::task_local! {
    static CLIENTS: crate::util::fetch::DownloadClients;
}

pub(crate) fn clients() -> Option<crate::util::fetch::DownloadClients> {
    CLIENTS.try_with(Clone::clone).ok().or_else(|| {
        crate::State::get_if_initialized().map(|state| state.download_clients())
    })
}

pub(crate) async fn with_clients<F: std::future::Future>(
    future: F,
) -> F::Output {
    match clients() {
        Some(clients) => CLIENTS.scope(clients, future).await,
        None => future.await,
    }
}

pub(crate) async fn with_snapshot<F: std::future::Future>(
    clients: crate::util::fetch::DownloadClients,
    future: F,
) -> F::Output {
    CLIENTS.scope(clients, future).await
}

pub(crate) fn fingerprint(
    config: &ProxyConfig,
    ignore_ssl_errors: bool,
) -> String {
    fingerprint_with_doh(
        config,
        ignore_ssl_errors,
        false,
        crate::util::download_dns::DEFAULT_DOH_SERVER,
    )
}

pub(crate) fn fingerprint_with_doh(
    config: &ProxyConfig,
    ignore_ssl_errors: bool,
    doh_enabled: bool,
    doh_server: &str,
) -> String {
    let mut digest = Sha256::new();
    for value in [
        config.mode.as_str(),
        config.url.trim(),
        &config.username,
        &config.password,
    ] {
        digest.update((value.len() as u64).to_le_bytes());
        digest.update(value.as_bytes());
    }
    digest.update([u8::from(ignore_ssl_errors)]);
    digest.update([u8::from(doh_enabled)]);
    digest.update((doh_server.len() as u64).to_le_bytes());
    digest.update(doh_server.as_bytes());
    format!("{:x}", digest.finalize())
}

pub(crate) fn scope(proxy: ProxyPolicy) -> String {
    clients()
        .map(|clients| match proxy {
            ProxyPolicy::System => clients.scope,
            ProxyPolicy::Direct => fingerprint(
                &ProxyConfig {
                    mode: crate::util::proxy::ProxyMode::None,
                    ..Default::default()
                },
                clients.ignore_ssl_errors,
            ),
        })
        .unwrap_or_else(|| format!("unconfigured-{proxy:?}"))
}

pub(crate) fn authority_key(authority: &str, proxy: ProxyPolicy) -> String {
    format!("{authority}|{}", scope(proxy))
}

pub(crate) fn resolver(proxy: ProxyPolicy) -> Arc<DownloadDnsResolver> {
    clients()
        .map(|clients| match proxy {
            ProxyPolicy::System => clients.system_dns,
            ProxyPolicy::Direct => clients.direct_dns,
        })
        .unwrap_or_else(|| {
            Arc::clone(&crate::util::fetch::DOWNLOAD_DNS_RESOLVER)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::proxy::ProxyMode;

    #[test]
    fn configuration_identity_includes_mode_endpoint_credentials_and_tls() {
        let base = ProxyConfig {
            mode: ProxyMode::Custom,
            url: "http://127.0.0.1:7890".into(),
            username: "user".into(),
            password: "secret".into(),
        };
        let original = fingerprint(&base, false);
        assert_eq!(original, fingerprint(&base.clone(), false));
        for changed in [
            ProxyConfig {
                mode: ProxyMode::None,
                ..base.clone()
            },
            ProxyConfig {
                url: "socks5://127.0.0.1:7891".into(),
                ..base.clone()
            },
            ProxyConfig {
                username: "another".into(),
                ..base.clone()
            },
            ProxyConfig {
                password: "another".into(),
                ..base.clone()
            },
        ] {
            assert_ne!(original, fingerprint(&changed, false));
        }
        assert_ne!(original, fingerprint(&base, true));
        assert!(!original.contains("secret"));
    }

    #[test]
    fn rebuilt_clients_have_separate_dns_and_connection_pools() {
        let config = ProxyConfig {
            mode: ProxyMode::None,
            ..Default::default()
        };
        let first = crate::util::fetch::DownloadClients::build(
            &config,
            false,
            true,
            crate::util::download_dns::DEFAULT_DOH_SERVER,
        )
        .unwrap();
        let second = crate::util::fetch::DownloadClients::build(
            &ProxyConfig {
                mode: ProxyMode::Custom,
                url: "http://127.0.0.1:7890".into(),
                ..Default::default()
            },
            false,
            true,
            crate::util::download_dns::DEFAULT_DOH_SERVER,
        )
        .unwrap();
        assert_ne!(first.scope, second.scope);
        assert!(!Arc::ptr_eq(&first.system_dns, &first.direct_dns));
        assert!(!Arc::ptr_eq(&first.system_dns, &second.system_dns));
        let snapshot = first.clone();
        assert!(Arc::ptr_eq(&first.system_dns, &snapshot.system_dns));
    }

    #[tokio::test]
    async fn request_snapshot_isolates_health_and_authority_budgets() {
        let route = crate::util::fetch::DownloadRoute {
            url: "https://proxy-isolation.invalid/file".into(),
            source: crate::util::fetch::DownloadRouteSource::Official,
            is_mirror: false,
            allow_sensitive_headers: true,
            supports_range: true,
            proxy: ProxyPolicy::System,
        };
        let mut keys = Vec::new();
        for port in [7890, 7891] {
            let clients = crate::util::fetch::DownloadClients::build(
                &ProxyConfig {
                    mode: ProxyMode::Custom,
                    url: format!("http://127.0.0.1:{port}"),
                    ..Default::default()
                },
                false,
                true,
                crate::util::download_dns::DEFAULT_DOH_SERVER,
            )
            .unwrap();
            keys.push(with_snapshot(clients, async {
				assert_eq!(super::super::native::h2_ineligible_reason(&route), Some(super::super::native::NativeH2IneligibleReason::ConfiguredProxy));
				super::super::native_reputation::record_success("other", "proxy-isolation.invalid:443", ProxyPolicy::System, 1.0, None);
				let mut permits = Vec::new();
				for _ in 0..8 { permits.push(super::super::native_budget::acquire(&route).await.unwrap()); }
				assert_eq!(super::super::native_budget::available(&route), 0);
				let key = super::super::route_health::route_health_key(&route, crate::util::fetch::ResourceClass::Other).unwrap();
				drop(permits);
				key
			}).await);
        }
        assert_ne!(keys[0], keys[1]);
    }
}
