//! Logical stream budgets for shared native HTTP/2 connections.
//!
//! HTTP/2 streams share one physical connection per authority, so they must
//! not consume the HTTP/1 connection permits held by `native_budget`.

use crate::util::fetch::{DownloadRoute, ProxyPolicy};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{
    AcquireError, OwnedSemaphorePermit, Semaphore, SemaphorePermit,
};
use tokio::time::{Duration, sleep};

const MAX_H2_STREAMS: usize = 128;
const MAX_H2_STREAMS_PER_AUTHORITY: usize = 32;
const MAX_ASSET_H2_STREAMS: usize = 96;
const MAX_ASSET_H2_STREAMS_PER_AUTHORITY: usize = 24;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AuthorityKey {
    authority: String,
    proxy: ProxyPolicy,
}

static AUTHORITY_BUDGETS: LazyLock<
    Mutex<HashMap<AuthorityKey, Arc<Semaphore>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static GLOBAL_BUDGET: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_H2_STREAMS)));
static ASSET_AUTHORITY_BUDGETS: LazyLock<
    Mutex<HashMap<AuthorityKey, Arc<Semaphore>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));
static ASSET_GLOBAL_BUDGET: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_ASSET_H2_STREAMS)));

pub(crate) struct H2StreamPermit {
    _global: OwnedSemaphorePermit,
    _authority: Option<OwnedSemaphorePermit>,
}

pub(crate) struct H2DownloadPermit<'a> {
    _stream: H2StreamPermit,
    _file: Option<SemaphorePermit<'a>>,
}

pub(crate) async fn acquire_download<'a>(
    route: &DownloadRoute,
    semaphore: Option<&'a crate::util::fetch::FetchSemaphore>,
    asset: bool,
) -> Result<H2DownloadPermit<'a>, AcquireError> {
    let stream = if asset {
        acquire_asset(route).await?
    } else {
        acquire(route).await?
    };
    let file = match semaphore {
        Some(semaphore) => Some(semaphore.0.acquire().await?),
        None => None,
    };
    Ok(H2DownloadPermit {
        _stream: stream,
        _file: file,
    })
}

fn budget(route: &DownloadRoute) -> Option<Arc<Semaphore>> {
    let authority = crate::util::fetch::url_authority(&route.url)?;
    let key = AuthorityKey {
        authority,
        proxy: route.proxy,
    };
    let mut budgets = AUTHORITY_BUDGETS.lock();
    if budgets.len() >= 256 {
        budgets.retain(|_, budget| Arc::strong_count(budget) > 1);
    }
    Some(
        budgets
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Semaphore::new(MAX_H2_STREAMS_PER_AUTHORITY))
            })
            .clone(),
    )
}

fn asset_budget(route: &DownloadRoute) -> Option<Arc<Semaphore>> {
    let authority = crate::util::fetch::url_authority(&route.url)?;
    let key = AuthorityKey {
        authority,
        proxy: route.proxy,
    };
    let mut budgets = ASSET_AUTHORITY_BUDGETS.lock();
    if budgets.len() >= 256 {
        budgets.retain(|_, budget| Arc::strong_count(budget) > 1);
    }
    Some(
        budgets
            .entry(key)
            .or_insert_with(|| {
                Arc::new(Semaphore::new(MAX_ASSET_H2_STREAMS_PER_AUTHORITY))
            })
            .clone(),
    )
}

pub(crate) async fn acquire(
    route: &DownloadRoute,
) -> Result<H2StreamPermit, AcquireError> {
    let authority_budget = budget(route);
    loop {
        let global = Arc::clone(&GLOBAL_BUDGET).acquire_owned().await?;
        let authority = match authority_budget.as_ref() {
            Some(budget) => match Arc::clone(budget).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    drop(global);
                    sleep(Duration::from_millis(5)).await;
                    continue;
                }
                Err(tokio::sync::TryAcquireError::Closed) => {
                    unreachable!("H2 authority budgets are never closed")
                }
            },
            None => None,
        };
        return Ok(H2StreamPermit {
            _global: global,
            _authority: authority,
        });
    }
}

/// Acquires a stream for the Minecraft asset batch. Assets retain a large
/// logical worker window, but use a separate physical stream pool so they
/// cannot consume the ordinary native content share.
pub(crate) async fn acquire_asset(
    route: &DownloadRoute,
) -> Result<H2StreamPermit, AcquireError> {
    let authority_budget = asset_budget(route);
    loop {
        let global = Arc::clone(&ASSET_GLOBAL_BUDGET).acquire_owned().await?;
        let authority = match authority_budget.as_ref() {
            Some(budget) => match Arc::clone(budget).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(tokio::sync::TryAcquireError::NoPermits) => {
                    drop(global);
                    sleep(Duration::from_millis(5)).await;
                    continue;
                }
                Err(tokio::sync::TryAcquireError::Closed) => {
                    unreachable!("asset authority budgets are never closed")
                }
            },
            None => None,
        };
        return Ok(H2StreamPermit {
            _global: global,
            _authority: authority,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::fetch::DownloadRouteSource;

    fn route() -> DownloadRoute {
        DownloadRoute {
            url: "https://h2-budget.example/file".to_string(),
            source: DownloadRouteSource::Official,
            is_mirror: false,
            allow_sensitive_headers: true,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        }
    }

    #[tokio::test]
    async fn authority_stream_budget_is_independent_from_connection_budget() {
        let route = route();
        let mut permits = Vec::new();
        for _ in 0..MAX_H2_STREAMS_PER_AUTHORITY {
            permits.push(acquire(&route).await.unwrap());
        }
        assert_eq!(budget(&route).unwrap().available_permits(), 0,);
        drop(permits);
        assert_eq!(
            budget(&route).unwrap().available_permits(),
            MAX_H2_STREAMS_PER_AUTHORITY,
        );
    }

    #[tokio::test]
    async fn asset_stream_budget_is_separate_and_bounded() {
        let route = DownloadRoute {
            url: "https://asset-h2-budget.example/file".to_string(),
            source: DownloadRouteSource::Bmclapi,
            is_mirror: true,
            allow_sensitive_headers: false,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        };
        let mut permits = Vec::new();
        for _ in 0..MAX_ASSET_H2_STREAMS_PER_AUTHORITY {
            permits.push(acquire_asset(&route).await.unwrap());
        }
        assert_eq!(asset_budget(&route).unwrap().available_permits(), 0,);
        drop(permits);
        assert_eq!(
            asset_budget(&route).unwrap().available_permits(),
            MAX_ASSET_H2_STREAMS_PER_AUTHORITY,
        );
        assert_eq!(
            budget(&route).unwrap().available_permits(),
            MAX_H2_STREAMS_PER_AUTHORITY
        );
    }
}
