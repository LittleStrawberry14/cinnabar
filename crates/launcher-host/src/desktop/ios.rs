//! Browser handoff through `UIApplication`, since iOS has no desktop opener or tools.

use block2::RcBlock;
use crossbeam_channel::bounded;
use objc2_foundation::{MainThreadMarker, NSOperationQueue, NSString, NSURL};
use objc2_ui_kit::UIApplication;

/// Opens `url` in the system browser and reports whether the handoff was accepted.
///
/// `UIApplication` is main-thread-only: sign-in callers on the main thread open directly, while
/// the generic opener's worker dispatches to the main queue, which the window's run loop drains
/// between frames, and blocks until the handoff completes.
pub(super) fn open_url(url: &str) -> bool {
    if let Some(marker) = MainThreadMarker::new() {
        return open_on_main(marker, url);
    }
    let (sender, receiver) = bounded(1);
    let block = RcBlock::new(move || {
        // Main-queue operations only ever run on the main thread.
        let marker = MainThreadMarker::new().expect("main-queue operations run on the main thread");
        let _ = sender.try_send(open_on_main(marker, &url));
    });
    // SAFETY: `mainQueue` and the block argument follow the generated bindings' contracts.
    unsafe {
        NSOperationQueue::mainQueue().addOperationWithBlock(&block);
    }
    receiver.recv().unwrap_or(false)
}

/// Opens `url` in the browser through the shared application.
fn open_on_main(marker: MainThreadMarker, url: &str) -> bool {
    // SAFETY: the selector only parses the string.
    let Some(url) = (unsafe { NSURL::URLWithString(&NSString::from_str(url)) }) else {
        tracing::warn!("not a link the system browser can open: {url}");
        return false;
    };
    open_url_selector(&UIApplication::sharedApplication(marker), &url)
}

/// Calls the shared application's browser handoff.
#[expect(
    deprecated,
    reason = "the synchronous selector still reports whether a browser accepted the link, \
        which keeps sign-in's manual recovery visible"
)]
fn open_url_selector(application: &UIApplication, url: &NSURL) -> bool {
    application.openURL(url)
}
