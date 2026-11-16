// SPDX-License-Identifier: AGPL-3.0-only

//! macOS specifics Tauri does not expose: App Nap, WebView throttling,
//! "app became active" (e.g. after a click on a notification), and a main-frame
//! navigation policy that runs *before* WebKit commits a document.

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_foundation::{NSActivityOptions, NSObjectProtocol, NSProcessInfo, NSString};
use objc2_web_kit::{
    WKNavigationAction, WKNavigationActionPolicy, WKNavigationDelegate, WKNavigationResponse,
    WKNavigationResponsePolicy, WKWebView,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

/// Disables App Nap for the lifetime of the process. Idle system sleep stays allowed.
pub fn disable_app_nap() {
    let info = NSProcessInfo::processInfo();
    let reason = NSString::from_str("PostWarden überwacht Outlook im Hintergrund");
    let token = info.beginActivityWithOptions_reason(
        NSActivityOptions::UserInitiatedAllowingIdleSystemSleep,
        &reason,
    );
    // The activity lasts as long as the token lives – i.e. forever.
    std::mem::forget(token);
}

/// `WKPreferences.inactiveSchedulingPolicy = .none` (macOS 14+), so timers and
/// observers keep running while the window is hidden. Returns false on older systems.
///
/// # Safety
/// `wkwebview` must be the `WKWebView*` handed out by `with_webview` (main thread).
pub unsafe fn disable_webview_throttling(wkwebview: *mut c_void) -> bool {
    const WK_INACTIVE_SCHEDULING_POLICY_NONE: isize = 2;
    let Some(webview) = (wkwebview as *const AnyObject).as_ref() else {
        return false;
    };
    let config: *const AnyObject = msg_send![webview, configuration];
    let Some(config) = config.as_ref() else {
        return false;
    };
    let prefs: *const AnyObject = msg_send![config, preferences];
    let Some(prefs) = prefs.as_ref() else {
        return false;
    };
    let supported: bool = msg_send![prefs, respondsToSelector: sel!(setInactiveSchedulingPolicy:)];
    if supported {
        let _: () =
            msg_send![prefs, setInactiveSchedulingPolicy: WK_INACTIVE_SCHEDULING_POLICY_NONE];
    }
    supported
}

/// Calls `f` whenever the app becomes active (Dock click, ⌘-Tab, click on a notification).
pub fn on_did_become_active(f: impl Fn() + Send + 'static) {
    use block2::RcBlock;
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    use std::ptr::NonNull;

    let center = NSNotificationCenter::defaultCenter();
    let name = NSString::from_str("NSApplicationDidBecomeActiveNotification");
    use objc2_foundation::NSOperationQueue;
    let block = RcBlock::new(move |_n: NonNull<NSNotification>| f());
    let main_queue = NSOperationQueue::mainQueue();
    // SAFETY:
    // - `center`, `name`, `main_queue` and `block` are valid, retained Objective-C
    //   objects for the duration of the call; `object: None` observes any sender.
    // - `queue: mainQueue` makes the center run the block on the main queue only, so
    //   `f` is never called concurrently or from another thread, whoever posts a
    //   notification of this name. (`f` only schedules work via Tauri handles.)
    // - The notification center copies (retains) the block, so dropping our `RcBlock`
    //   reference afterwards is fine. The returned observer token is leaked on purpose:
    //   the observation must live for the whole process and is never removed, so the
    //   block (and the captured `f`) is never freed while it can still be called.
    let observer = unsafe {
        center.addObserverForName_object_queue_usingBlock(
            Some(&name),
            None,
            Some(&main_queue),
            &block,
        )
    };
    std::mem::forget(observer);
}

/// Main-frame policy: (target URL, URL of the current document) → allow?
pub type MainFramePolicy = Box<dyn Fn(&str, Option<&str>) -> bool>;
/// What to do with a main-frame response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDecision {
    /// Normal handling (wry).
    Forward,
    /// Save as a download (attachment from a download-only host).
    Download,
    /// Do not show it here (document from a download-only host); already handed on.
    Cancel,
}

/// Main-frame response policy: (URL, `Content-Disposition: attachment`, WebKit can
/// show the MIME type) → decision.
pub type ResponsePolicy = Box<dyn Fn(&str, bool, bool) -> ResponseDecision>;

pub struct GuardIvars {
    inner: Retained<ProtocolObject<dyn WKNavigationDelegate>>,
    policy: MainFramePolicy,
    response_policy: ResponsePolicy,
}

thread_local! {
    /// Installed guards by window label (main thread only). `navigationDelegate` is a
    /// weak property, so the guard must be owned here – and released when its window
    /// is destroyed, together with the wry delegate (and web view) it retains.
    static GUARDS: RefCell<HashMap<String, Retained<NavigationGuard>>> =
        RefCell::new(HashMap::new());
}

define_class!(
    /// Navigation delegate placed in front of wry's: it decides main-frame navigations
    /// (links, redirects, `location` changes) before WebKit loads anything, and forwards
    /// everything else – including every other delegate method – to wry unchanged.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = GuardIvars]
    struct NavigationGuard;

    unsafe impl NSObjectProtocol for NavigationGuard {}

    unsafe impl WKNavigationDelegate for NavigationGuard {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn decide_policy(
            &self,
            webview: &WKWebView,
            action: &WKNavigationAction,
            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,
        ) {
            // SAFETY: plain property reads on objects WebKit hands us on the main thread.
            let (main_frame, download, url, current) = unsafe {
                let main_frame = action.targetFrame().is_some_and(|f| f.isMainFrame());
                let download = action.respondsToSelector(sel!(shouldPerformDownload))
                    && action.shouldPerformDownload();
                let url = action
                    .request()
                    .URL()
                    .and_then(|u| u.absoluteString())
                    .map(|s| s.to_string());
                let current = webview
                    .URL()
                    .and_then(|u| u.absoluteString())
                    .map(|s| s.to_string());
                (main_frame, download, url, current)
            };
            // Downloads keep wry's path (checked in `on_download`); iframes are wry's too.
            if main_frame && !download {
                if let Some(url) = url {
                    if !(self.ivars().policy)(&url, current.as_deref()) {
                        handler.call((WKNavigationActionPolicy::Cancel,));
                        return;
                    }
                }
            }
            // SAFETY: wry's delegate implements this method (it is the one WebKit used
            // before); arguments are passed through unchanged.
            unsafe {
                self.ivars()
                    .inner
                    .webView_decidePolicyForNavigationAction_decisionHandler(webview, action, handler)
            }
        }

        /// A main-frame response from a download-only host (attachments) is saved as a
        /// download if it is one (attachment header or not displayable); a displayable
        /// document from there is never shown in the window.
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn decide_response(
            &self,
            webview: &WKWebView,
            response: &WKNavigationResponse,
            handler: &block2::DynBlock<dyn Fn(WKNavigationResponsePolicy)>,
        ) {
            // SAFETY: plain property reads on objects WebKit hands us on the main thread;
            // `valueForHTTPHeaderField:` is only sent if the response implements it
            // (NSHTTPURLResponse).
            let (main_frame, url, attachment, can_show) = unsafe {
                let r = response.response();
                let url = r.URL().and_then(|u| u.absoluteString()).map(|s| s.to_string());
                let has_headers: bool =
                    msg_send![&*r, respondsToSelector: sel!(valueForHTTPHeaderField:)];
                let attachment = has_headers && {
                    let name = NSString::from_str("Content-Disposition");
                    let value: Option<Retained<NSString>> =
                        msg_send![&*r, valueForHTTPHeaderField: &*name];
                    value.is_some_and(|v| {
                        v.to_string().trim_start().to_ascii_lowercase().starts_with("attachment")
                    })
                };
                (response.isForMainFrame(), url, attachment, response.canShowMIMEType())
            };
            if main_frame {
                if let Some(url) = url {
                    match (self.ivars().response_policy)(&url, attachment, can_show) {
                        ResponseDecision::Forward => {}
                        ResponseDecision::Download => {
                            handler.call((WKNavigationResponsePolicy::Download,));
                            return;
                        }
                        ResponseDecision::Cancel => {
                            handler.call((WKNavigationResponsePolicy::Cancel,));
                            return;
                        }
                    }
                }
            }
            // SAFETY: wry's delegate implements this method; arguments unchanged.
            unsafe {
                self.ivars()
                    .inner
                    .webView_decidePolicyForNavigationResponse_decisionHandler(webview, response, handler)
            }
        }
    }

    impl NavigationGuard {
        /// Claims exactly the methods wry's delegate implements (WebKit caches this
        /// when the delegate is set), plus our own.
        #[unsafe(method(respondsToSelector:))]
        fn responds_to_selector(&self, selector: Sel) -> bool {
            // SAFETY: NSObject's implementation of the same selector.
            let own: bool = unsafe { msg_send![super(self), respondsToSelector: selector] };
            own || self.ivars().inner.respondsToSelector(selector)
        }

        /// Every message we do not implement goes to wry's delegate.
        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target(&self, _selector: Sel) -> *mut AnyObject {
            Retained::as_ptr(&self.ivars().inner) as *mut AnyObject
        }
    }
);

/// Puts [`NavigationGuard`] in front of the webview's current (wry) navigation delegate
/// and keeps it under `label` until [`release_navigation_guard`]. Returns false if
/// there is no delegate or we are not on the main thread.
///
/// # Safety
/// `wkwebview` must be the `WKWebView*` handed out by `with_webview` (main thread).
pub unsafe fn install_navigation_guard(
    wkwebview: *mut c_void,
    label: &str,
    policy: MainFramePolicy,
    response_policy: ResponsePolicy,
) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(webview) = (wkwebview as *const WKWebView).as_ref() else {
        return false;
    };
    let Some(inner) = webview.navigationDelegate() else {
        return false;
    };
    let guard = NavigationGuard::alloc(mtm).set_ivars(GuardIvars {
        inner,
        policy,
        response_policy,
    });
    let guard: Retained<NavigationGuard> = msg_send![super(guard), init];
    webview.setNavigationDelegate(Some(ProtocolObject::from_ref(&*guard)));
    GUARDS.with(|g| g.borrow_mut().insert(label.to_string(), guard));
    true
}

/// Releases the guard of a destroyed window (and with it the wry delegate it retains).
pub fn release_navigation_guard(label: &str) {
    if MainThreadMarker::new().is_some() {
        GUARDS.with(|g| g.borrow_mut().remove(label));
    }
}
