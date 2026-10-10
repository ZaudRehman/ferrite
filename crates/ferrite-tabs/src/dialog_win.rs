//! Windows script dialog interception. Public surfaces only, no fork.
//! Other platforms keep engine defaults. Every handler path either stashes
//! the deferral for the shell loop or completes it inline as cancelled.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use uuid::Uuid;
use webview2_com::{
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_SCRIPT_DIALOG_KIND, ICoreWebView2Deferral,
        ICoreWebView2ScriptDialogOpeningEventArgs,
    },
    ScriptDialogOpeningEventHandler, take_pwstr,
};
use windows::core::{HSTRING, PWSTR};
use wry::WebViewExtWindows;

use super::dialog::{DIALOG_MESSAGE_CAP, DialogKind, DialogOpened, DialogRequest, origin_key};

struct Outstanding {
    tab_id: Uuid,
    args: ICoreWebView2ScriptDialogOpeningEventArgs,
    deferral: ICoreWebView2Deferral,
}

// SAFETY: WebView2 runs everything, event callbacks and the controller
// lifetime, on the creating UI thread (documented in webview2-com
// itself: "runs everything on the UI thread, including callbacks").
// Ferrite creates every controller on the shell loop thread and answers
// there too, so these interfaces never actually cross threads. The impl
// exists only so the stash can live behind the shared sink; a contract
// violation would surface as a failed Complete() (logged), never as
// memory unsafety (all calls go through the COM vtable).
unsafe impl Send for Outstanding {}
unsafe impl Sync for Outstanding {}

impl std::fmt::Debug for Outstanding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outstanding")
            .field("tab_id", &self.tab_id)
            .finish_non_exhaustive()
    }
}

/// Shared handle between the COM event thread and the shell loop: the
/// event side stashes deferrals and posts [`DialogOpened`]; the loop side
/// completes them. Suppression fast-path lives here too so a checked box
/// silences the origin without another round-trip.
#[derive(Clone)]
pub struct DialogSink {
    tx: std::sync::mpsc::Sender<DialogOpened>,
    suppressed: Arc<Mutex<HashSet<String>>>,
    outstanding: Arc<Mutex<HashMap<u64, Outstanding>>>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for DialogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DialogSink")
            .field("has_waker", &self.waker.is_some())
            .finish_non_exhaustive()
    }
}

impl DialogSink {
    pub fn new(
        tx: std::sync::mpsc::Sender<DialogOpened>,
        waker: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self {
            tx,
            suppressed: Arc::new(Mutex::new(HashSet::new())),
            outstanding: Arc::new(Mutex::new(HashMap::new())),
            waker,
        }
    }

    pub fn set_suppressed(&self, origin: &str, suppress: bool) {
        if let Ok(mut set) = self.suppressed.lock() {
            if suppress {
                set.insert(origin.to_owned());
            } else {
                set.remove(origin);
            }
        }
    }

    /// Complete a stashed dialog: accept (with prompt result text), or
    /// cancel (script resumes as dismissed). Always completes the
    /// deferral on the accept path too, best effort first, unblock last.
    /// Returns false when `seq` is unknown (stale/duplicate response).
    pub fn complete(&self, seq: u64, accept: bool, result_text: Option<&str>) -> bool {
        let outstanding = match self.outstanding.lock() {
            Ok(mut map) => map.remove(&seq),
            Err(_) => return false,
        };
        let Some(o) = outstanding else {
            return false;
        };
        unsafe {
            if accept {
                if let Some(text) = result_text {
                    let capped: String = text.chars().take(DIALOG_MESSAGE_CAP).collect();
                    let h = HSTRING::from(capped.as_str());
                    if o.args.SetResultText(&h).is_err() {
                        let _ = o.deferral.Complete();
                        return false;
                    }
                }
                let _ = o.args.Accept();
            }
            o.deferral.Complete().is_ok()
        }
    }

    /// Cancel every outstanding dialog for a tab (close / discard /
    /// navigate-away). Returns how many scripts were unblocked.
    pub fn cancel_tab(&self, tab_id: Uuid) -> usize {
        let seqs: Vec<u64> = match self.outstanding.lock() {
            Ok(map) => map
                .iter()
                .filter(|(_, o)| o.tab_id == tab_id)
                .map(|(seq, _)| *seq)
                .collect(),
            Err(_) => return 0,
        };
        let mut done = 0;
        for seq in seqs {
            if self.complete(seq, false, None) {
                done += 1;
            }
        }
        done
    }
}

/// Read one out-param string, freeing the COM buffer. `None` on failure.
/// Callers treat this as "no text", never as fatal.
unsafe fn get_str(call: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> Option<String> {
    let mut p = PWSTR::null();
    call(&mut p).ok()?;
    Some(take_pwstr(p))
}

/// Complete a dialog inline as cancelled (suppressed origins, unknown
/// kinds, broken handoffs). Best effort; the script must not hang.
fn cancel_inline(args: &ICoreWebView2ScriptDialogOpeningEventArgs) {
    if let Ok(d) = unsafe { args.GetDeferral() } {
        let _ = unsafe { d.Complete() };
    }
}

/// Attach script-dialog interception to one built webview. Disables the
/// engine defaults (so scam-styled native boxes never appear) and routes
/// every alert/confirm/prompt/beforeunload through `sink`.
pub fn attach_script_dialogs(
    webview: &wry::WebView,
    tab_id: Uuid,
    sink: &DialogSink,
) -> Result<(), String> {
    let core = webview.webview();
    let settings = unsafe { core.Settings() }.map_err(|e| format!("dialog settings: {e:?}"))?;
    unsafe { settings.SetAreDefaultScriptDialogsEnabled(false) }
        .map_err(|e| format!("dialog disable defaults: {e:?}"))?;

    let tx = sink.tx.clone();
    let suppressed = Arc::clone(&sink.suppressed);
    let outstanding = Arc::clone(&sink.outstanding);
    let waker = sink.waker.clone();
    let handler = ScriptDialogOpeningEventHandler::create(Box::new(
        move |_sender, args: Option<ICoreWebView2ScriptDialogOpeningEventArgs>| {
            let Some(args) = args else {
                return Ok(());
            };
            // Extract first: a broken event unblocks as cancelled below.
            let (kind_raw, uri, message, default_text) = unsafe {
                let mut kind = COREWEBVIEW2_SCRIPT_DIALOG_KIND(0);
                let kind_ok = args.Kind(&mut kind).is_ok();
                let uri = get_str(|p| args.Uri(p)).unwrap_or_default();
                let message = get_str(|p| args.Message(p)).unwrap_or_default();
                let default_text = get_str(|p| args.DefaultText(p)).unwrap_or_default();
                if !kind_ok {
                    cancel_inline(&args);
                    return Ok(());
                }
                (kind.0, uri, message, default_text)
            };
            let kind = match DialogKind::from_i32(kind_raw) {
                Some(k) => k,
                None => {
                    cancel_inline(&args);
                    return Ok(());
                }
            };
            // Suppression fast-path (alert/confirm/prompt only, a
            // silenced beforeunload would trap navigation with no UI, so
            // it always goes to the shell like an unsuppressed dialog).
            let suppressed_hit = kind != DialogKind::BeforeUnload
                && suppressed
                    .lock()
                    .map(|set| set.contains(&origin_key(&uri)))
                    .unwrap_or(false);
            if suppressed_hit {
                cancel_inline(&args);
                return Ok(());
            }
            let Ok(deferral) = (unsafe { args.GetDeferral() }) else {
                return Ok(());
            };
            // Random frame id: overlay posts pair by `seq`, and a
            // sequential counter would let one page answer another tab's
            // dialog by guessing.
            let seq = Uuid::new_v4().as_u128() as u64;
            if outstanding
                .lock()
                .map(|mut map| {
                    map.insert(
                        seq,
                        Outstanding {
                            tab_id,
                            args: args.clone(),
                            deferral,
                        },
                    )
                })
                .is_err()
            {
                cancel_inline(&args);
                return Ok(());
            }
            let opened = DialogOpened {
                req: DialogRequest::new(seq, tab_id, kind, &message, &uri, &default_text),
            };
            if tx.send(opened).is_err() {
                // Shell gone: unblock the script as cancelled.
                if let Ok(mut map) = outstanding.lock()
                    && let Some(o) = map.remove(&seq)
                {
                    let _ = unsafe { o.deferral.Complete() };
                }
            } else if let Some(w) = waker.as_ref() {
                w();
            }
            Ok(())
        },
    ));
    let mut token = 0i64;
    unsafe { core.add_ScriptDialogOpening(&handler, &mut token) }
        .map_err(|e| format!("dialog attach: {e:?}"))?;
    Ok(())
}
