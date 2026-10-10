//! Bookmark link sweep. Explicit user request only, never automatic.
//! Results stream as each check completes. Transport is `direct`, and GETs
//! with a tiny cap stand in for HEAD.
//!
//! ponytail: semaphore plus spawn is the pool. No new dependency.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::Sender;

use crate::direct::direct_get;

/// One checked URL. `ok` means 2xx or 3xx. Dead links are surfaced for
/// review, never auto deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkStatus {
    pub url: String,
    pub ok: bool,
    pub status: Option<u16>,
    pub checked_at: i64,
}

pub const DEFAULT_LINKCHECK_CONCURRENCY: usize = 4;
pub const MAX_LINKCHECK_CONCURRENCY: usize = 16;
/// Body cap per probe. Status line only.
pub const LINKCHECK_MAX_BODY: usize = 8 * 1024;
/// Batch cap. Bounds large collections.
pub const MAX_LINKCHECK_URLS: usize = 5000;

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Check one URL. No side effects, no caching.
pub async fn check_one_link(url: &str) -> LinkStatus {
    let checked_at = now_millis();
    match direct_get(url, LINKCHECK_MAX_BODY).await {
        Some(resp) => {
            let ok = (200..400).contains(&resp.status);
            LinkStatus {
                url: url.to_owned(),
                ok,
                status: Some(resp.status),
                checked_at,
            }
        }
        None => LinkStatus {
            url: url.to_owned(),
            ok: false,
            status: None,
            checked_at,
        },
    }
}

/// Sweep with dedupe and caps. Streams one status per URL. Returns probed
/// count. A dead `tx` stops the sweep early.
pub async fn check_bookmark_links(
    urls: Vec<String>,
    concurrency: usize,
    tx: Sender<LinkStatus>,
) -> usize {
    let concurrency = concurrency.clamp(1, MAX_LINKCHECK_CONCURRENCY);
    let mut seen = std::collections::HashSet::new();
    let mut queue: Vec<String> = Vec::new();
    for u in urls {
        if queue.len() >= MAX_LINKCHECK_URLS {
            break;
        }
        if seen.insert(u.clone()) {
            queue.push(u);
        }
    }
    let total = queue.len();
    if total == 0 {
        return 0;
    }
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut done_rx = {
        let (done_tx, done_rx) = tokio::sync::mpsc::channel::<LinkStatus>(total);
        for url in queue {
            let permit_tx = done_tx.clone();
            let sem = Arc::clone(&sem);
            tokio::spawn(async move {
                let _permit = sem.acquire_owned().await;
                let status = check_one_link(&url).await;
                let _ = permit_tx.send(status).await;
            });
        }
        drop(done_tx);
        done_rx
    };
    let mut probed = 0;
    while let Some(status) = done_rx.recv().await {
        probed += 1;
        if tx.send(status).is_err() {
            break;
        }
    }
    probed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::bounded;

    #[tokio::test]
    async fn invalid_urls_report_dead_without_network() {
        // Non-HTTP schemes and unparsable URLs fail inside direct_get
        // before any socket opens, hermetic, no network needed.
        let s = check_one_link("ferrite://newtab").await;
        assert!(!s.ok && s.status.is_none());
        let s = check_one_link("not a url").await;
        assert!(!s.ok && s.status.is_none());
    }

    #[tokio::test]
    async fn empty_batch_probes_nothing() {
        let (tx, _rx) = bounded::<LinkStatus>(8);
        assert_eq!(check_bookmark_links(vec![], 4, tx).await, 0);
    }

    #[tokio::test]
    async fn dedupes_and_streams_incrementally() {
        let (tx, rx) = bounded::<LinkStatus>(16);
        let urls = vec![
            "ferrite://a".to_owned(),
            "ferrite://a".to_owned(),
            "not a url".to_owned(),
        ];
        assert_eq!(check_bookmark_links(urls, 4, tx).await, 2);
        let got: Vec<LinkStatus> = rx.try_iter().collect();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|s| !s.ok));
    }

    #[tokio::test]
    async fn concurrency_clamped_not_rejected() {
        let (tx, _rx) = bounded::<LinkStatus>(8);
        // 0 and huge both clamp into range instead of erroring.
        assert_eq!(
            check_bookmark_links(vec!["ferrite://x".to_owned()], 0, tx.clone()).await,
            1
        );
        let (tx2, _rx2) = bounded::<LinkStatus>(8);
        assert_eq!(
            check_bookmark_links(vec!["ferrite://x".to_owned()], 1_000_000, tx2).await,
            1
        );
    }
}
