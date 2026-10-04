//! Shared authority stream admission; every stream consumes one global transfer weight.

use crate::util::fetch::{DownloadRoute, ProxyPolicy};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{
    AcquireError, OwnedSemaphorePermit, Semaphore, SemaphorePermit,
};

const MAX_H2_STREAMS_PER_AUTHORITY: usize = 32;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AuthorityKey {
    authority: String,
    proxy: ProxyPolicy,
}

static AUTHORITY_BUDGETS: LazyLock<
    Mutex<HashMap<AuthorityKey, Arc<Semaphore>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct H2StreamPermit {
    _authority: Option<OwnedSemaphorePermit>,
}

pub(crate) struct H2DownloadPermit<'a> {
    _stream: H2StreamPermit,
    _file: Option<SemaphorePermit<'a>>,
}

pub(crate) async fn acquire_download<'a>(
    route: &DownloadRoute,
    semaphore: Option<&'a crate::util::fetch::FetchSemaphore>,
    _asset: bool,
) -> Result<H2DownloadPermit<'a>, AcquireError> {
    let stream = acquire(route).await?;
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
        authority: super::proxy_context::authority_key(&authority, route.proxy),
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

pub(crate) async fn acquire(
    route: &DownloadRoute,
) -> Result<H2StreamPermit, AcquireError> {
    Ok(H2StreamPermit {
        _authority: match budget(route) {
            Some(budget) => Some(budget.acquire_owned().await?),
            None => None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::fetch::{DownloadRouteSource, FetchSemaphore};

    fn route(host: &str) -> DownloadRoute {
        DownloadRoute {
            url: format!("https://{host}/file"),
            source: DownloadRouteSource::Official,
            is_mirror: false,
            allow_sensitive_headers: true,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        }
    }

    #[tokio::test]
    async fn saturated_authority_does_not_reserve_global_weights() {
        let busy = route("busy-weight.invalid");
        let other = route("other-weight.invalid");
        let global = FetchSemaphore(Semaphore::new(2));
        let mut held = Vec::new();
        for _ in 0..MAX_H2_STREAMS_PER_AUTHORITY {
            held.push(acquire(&busy).await.unwrap());
        }
        let pending = acquire_download(&busy, Some(&global), false);
        tokio::pin!(pending);
        assert!(futures::poll!(pending.as_mut()).is_pending());
        assert_eq!(global.0.available_permits(), 2);
        let available =
            acquire_download(&other, Some(&global), true).await.unwrap();
        assert_eq!(global.0.available_permits(), 1);
        drop(available);
        drop(pending);
        drop(held);
        assert_eq!(global.0.available_permits(), 2);
    }

    #[tokio::test]
    async fn ranges_and_assets_share_the_same_global_weights() {
        let global = FetchSemaphore(Semaphore::new(3));
        let target = route("shared-weights.invalid");
        let first = acquire_download(&target, Some(&global), false)
            .await
            .unwrap();
        let second = acquire_download(&target, Some(&global), false)
            .await
            .unwrap();
        let third = acquire_download(&target, Some(&global), true)
            .await
            .unwrap();
        assert_eq!(global.0.available_permits(), 0);
        drop((first, second, third));
        assert_eq!(global.0.available_permits(), 3);
    }
}
