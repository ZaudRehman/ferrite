//! Spellcheck dictionary provisioning worker. Page-initiated dictionary
//! fetches stay blocked at the proxy; missing dictionaries for the active
//! languages are fetched once here through the strict direct client (same
//! discipline as filter/threat refreshes: bare GET, no cookies, no query,
//! no user agent) and stored atomically in the bundle dir. Failures are
//! fail-soft: spellcheck continues without that dictionary.

/// Per-tag fetch report for the tick drain.
pub struct DictDone {
    pub tag: String,
    pub ok: bool,
}

/// Fetch every tag (usually the missing set) on a worker thread. Reports
/// per tag; the tick drain logs. Never blocks the winit loop.
pub fn spawn_dict_fetch(tags: Vec<String>, tx: std::sync::mpsc::Sender<DictDone>) {
    std::thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            for tag in tags {
                let _ = tx.send(DictDone { tag, ok: false });
            }
            return;
        };
        let dir = ferrite_privacy::dict_bundle_dir();
        for tag in tags {
            let ok = rt.block_on(fetch_and_store(&dir, &tag));
            let _ = tx.send(DictDone { tag, ok });
        }
    });
}

async fn fetch_and_store(dir: &std::path::Path, tag: &str) -> bool {
    let Some(url) = ferrite_privacy::dict_url(tag) else {
        return false;
    };
    let Some(bytes) = ferrite_privacy::updater::fetch_dict_bytes(&url).await else {
        return false;
    };
    ferrite_privacy::store_dict_bytes(dir, tag, &bytes)
}
