//! Per-authority physical connection admission; transfer weights use the shared download semaphore.

use crate::util::fetch::{DownloadRoute, ProxyPolicy};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

const MAX_CONNECTIONS_PER_AUTHORITY: usize = 32;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct AuthorityKey {
    authority: String,
    proxy: ProxyPolicy,
}

static AUTHORITY_BUDGETS: LazyLock<
    Mutex<HashMap<AuthorityKey, Arc<Semaphore>>>,
> = LazyLock::new(|| Mutex::new(HashMap::new()));

pub(crate) struct NativeBudgetPermit {
    _authority: Option<OwnedSemaphorePermit>,
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
                Arc::new(Semaphore::new(MAX_CONNECTIONS_PER_AUTHORITY))
            })
            .clone(),
    )
}

pub(crate) async fn acquire(
    route: &DownloadRoute,
) -> Result<NativeBudgetPermit, tokio::sync::AcquireError> {
    let authority = match budget(route) {
        Some(budget) => Some(budget.acquire_owned().await?),
        None => None,
    };
    Ok(NativeBudgetPermit {
        _authority: authority,
    })
}

pub(crate) async fn acquire_many(
    route: &DownloadRoute,
    count: usize,
) -> Result<Vec<NativeBudgetPermit>, tokio::sync::AcquireError> {
    let mut authority = match budget(route) {
        Some(budget) => Some(
            budget
                .acquire_many_owned(
                    count.min(MAX_CONNECTIONS_PER_AUTHORITY) as u32
                )
                .await?,
        ),
        None => None,
    };
    Ok((0..count.min(MAX_CONNECTIONS_PER_AUTHORITY))
        .map(|_| NativeBudgetPermit {
            _authority: authority.as_mut().map(|permit| {
                permit
                    .split(1)
                    .expect("authority batch contains enough permits")
            }),
        })
        .collect())
}

pub(crate) fn try_acquire(
    route: &DownloadRoute,
) -> Result<NativeBudgetPermit, TryAcquireError> {
    Ok(NativeBudgetPermit {
        _authority: match budget(route) {
            Some(budget) => Some(budget.try_acquire_owned()?),
            None => None,
        },
    })
}

pub(crate) fn available(route: &DownloadRoute) -> usize {
    budget(route)
        .map(|budget| budget.available_permits())
        .unwrap_or(MAX_CONNECTIONS_PER_AUTHORITY)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::fetch::DownloadRouteSource;

    fn route() -> DownloadRoute {
        DownloadRoute {
            url: "https://budget.example/file".to_string(),
            source: DownloadRouteSource::Official,
            is_mirror: false,
            allow_sensitive_headers: true,
            supports_range: true,
            proxy: ProxyPolicy::Direct,
        }
    }

    #[tokio::test]
    async fn authority_budget_is_bounded() {
        let route = route();
        let mut permits = Vec::new();
        for _ in 0..MAX_CONNECTIONS_PER_AUTHORITY {
            permits.push(acquire(&route).await.unwrap());
        }
        assert!(matches!(
            try_acquire(&route),
            Err(TryAcquireError::NoPermits)
        ));
        drop(permits);
        assert!(try_acquire(&route).is_ok());
    }
}
