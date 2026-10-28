//! Document picking through `UIDocumentPickerViewController`, since iOS has no picker tools.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use block2::RcBlock;
use crossbeam_channel::{Sender, bounded};
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2::{ClassType, DeclaredClass, declare_class, msg_send_id, mutability};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSOperationQueue, NSString, NSURL};
use objc2_ui_kit::{
    UIApplication, UIDocumentPickerDelegate, UIDocumentPickerViewController, UIViewController,
};
use objc2_uniform_type_identifiers::UTType;

use super::{Filter, PickedFile};

/// What the picker reported once the user closed it.
enum Selection {
    Picked(Vec<Retained<NSURL>>),
    Cancelled,
    Failed(&'static str),
}

/// Only one picker may block a worker at a time, mirroring the Android activity's guard.
static PICKER_OPEN: AtomicBool = AtomicBool::new(false);
/// Distinguishes each pick's private copies, as Android's import cache does.
static SELECTIONS: AtomicU64 = AtomicU64::new(0);

/// Presents the document picker on the main thread, blocks until the user closes it, and copies
/// the picks into private storage where the importer reads them by path. Returns an empty list on
/// cancellation.
///
/// Documents outside the app's container are security-scoped: access opens while one is copied.
/// Unknown extensions such as `.wasm` keep every document visible, mirroring Android's wildcard
/// MIME type, and `selected` checks extensions instead. A document past `max_bytes` fails the
/// selection without filling storage.
pub(super) fn pick_documents(multiple: bool, filter: &Filter) -> Result<Vec<PickedFile>, String> {
    let _open =
        OpenGuard::new().ok_or_else(|| "Another file picker is already open.".to_owned())?;
    let selection = SELECTIONS.fetch_add(1, Ordering::Relaxed);
    let imports = std::env::temp_dir().join("imports");
    // Android reclaims its cache directory at any time; iOS keeps the temporary directory, so
    // the first selection of a process clears earlier sessions' copies.
    if selection == 0 {
        let _ = std::fs::remove_dir_all(&imports);
    }
    let directory = imports.join(selection.to_string());

    let (sender, receiver) = bounded(1);
    let types = document_types(filter.extensions);
    let block = RcBlock::new(move || {
        // Main-queue operations only ever run on the main thread.
        let marker = MainThreadMarker::new().expect("main-queue operations run on the main thread");
        present(marker, multiple, &types, sender);
    });
    // SAFETY: `mainQueue` and the block argument follow the generated bindings' contracts.
    unsafe {
        NSOperationQueue::mainQueue().addOperationWithBlock(&block);
    }

    match receiver.recv() {
        Ok(Selection::Picked(urls)) => copy_documents(&urls, &directory, filter),
        // A picker released without reporting reads as cancellation.
        Ok(Selection::Cancelled) | Err(_) => Ok(Vec::new()),
        Ok(Selection::Failed(reason)) => Err(super::unavailable(reason, filter)),
    }
}

/// Releases the process-wide picker slot on every path out of `pick_documents`.
struct OpenGuard;

impl OpenGuard {
    fn new() -> Option<Self> {
        PICKER_OPEN
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
            .then_some(Self)
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        PICKER_OPEN.store(false, Ordering::Release);
    }
}

/// Maps the import extensions to unified type identifiers.
///
/// Providers may not register custom extensions such as `.wasm`, so lists with unknown
/// extensions fall back to `public.data`, which keeps every document visible while `selected`
/// checks extensions.
fn document_types(extensions: &'static [&'static str]) -> Retained<NSArray<UTType>> {
    let mapped: Option<Vec<Retained<UTType>>> = extensions
        .iter()
        .map(|extension| unsafe {
            // SAFETY: the selector only looks the extension up in the system registry.
            UTType::typeWithFilenameExtension(&NSString::from_str(extension))
        })
        .collect();
    let types = mapped.filter(|types| !types.is_empty()).unwrap_or_else(|| {
        vec![
            // SAFETY: `public.data` is a system-declared identifier.
            unsafe { UTType::typeWithIdentifier(&NSString::from_str("public.data")) }
                .expect("public.data is always declared"),
        ]
    });
    NSArray::from_id_slice(&types)
}

/// Shows the picker over the game's window and hands outcomes to the waiting worker.
fn present(
    marker: MainThreadMarker,
    multiple: bool,
    types: &NSArray<UTType>,
    sender: Sender<Selection>,
) {
    // SAFETY: the allocated picker is initialized with the content types before use.
    let picker = unsafe {
        UIDocumentPickerViewController::initForOpeningContentTypes(
            marker.alloc::<UIDocumentPickerViewController>(),
            types,
        )
    };
    // SAFETY: plain boolean setter.
    unsafe { picker.setAllowsMultipleSelection(multiple) };

    // The picker holds its delegate weakly; the association keeps ours alive with it.
    let delegate = PickerDelegate::new(marker, sender.clone());
    static DELEGATE_KEY: u8 = 0;
    // SAFETY: the association retains the delegate until the picker is released, which is after
    // the presentation ends.
    unsafe {
        objc2::ffi::objc_setAssociatedObject(
            Retained::as_ptr(&picker) as *mut objc2::ffi::objc_object,
            &DELEGATE_KEY as *const u8 as *const std::ffi::c_void,
            Retained::as_ptr(&delegate) as *mut objc2::ffi::objc_object,
            objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        picker.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    }

    let Some(host) = top_view_controller(marker) else {
        let _ = sender.try_send(Selection::Failed("no window could present the picker"));
        return;
    };
    let completion = RcBlock::new({
        // The block outlives this call, so it holds its own strong reference.
        let picker = picker.clone();
        move || {
            // A picker that UIKit refused to present never reaches its delegate, and only the
            // delegate reports successes and cancels.
            // SAFETY: reading whether a live view controller is being presented.
            let presented = unsafe { picker.presentingViewController() };
            if presented.is_none() {
                let _ =
                    sender.try_send(Selection::Failed("no view controller presented the picker"));
            }
        }
    });
    // SAFETY: presenting over a live view controller with a valid completion block.
    unsafe { host.presentViewController_animated_completion(&picker, true, Some(&completion)) };
}

/// The topmost view controller: the key window's root, unwrapping any active presentation.
fn top_view_controller(marker: MainThreadMarker) -> Option<Retained<UIViewController>> {
    let application = UIApplication::sharedApplication(marker);
    #[expect(
        deprecated,
        reason = "the key window is the game's own winit window, and scene-based window lookup \
            would need UIKit integration the launcher does not have"
    )]
    let window = unsafe { application.keyWindow() }?;
    let mut current = window.rootViewController()?;
    // SAFETY: reading the presentation chain of a live view controller.
    while let Some(presented) = unsafe { current.presentedViewController() } {
        current = presented;
    }
    Some(current)
}

/// One pick's outcomes, reported on the main thread while the worker blocks on the channel.
struct PickerState {
    sender: Sender<Selection>,
}

declare_class!(
    /// Reports one document picker's outcome once; the picker holds it weakly.
    struct PickerDelegate;

    unsafe impl ClassType for PickerDelegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "CinnabarPickerDelegate";
    }

    impl DeclaredClass for PickerDelegate {
        type Ivars = PickerState;
    }

    unsafe impl NSObjectProtocol for PickerDelegate {}

    unsafe impl UIDocumentPickerDelegate for PickerDelegate {
        #[method(documentPicker:didPickDocumentsAtURLs:)]
        fn did_pick_documents(
            &self,
            _picker: &UIDocumentPickerViewController,
            urls: &NSArray<NSURL>,
        ) {
            let _ = self
                .ivars()
                .sender
                .try_send(Selection::Picked(urls.to_vec_retained()));
        }

        #[method(documentPickerWasCancelled:)]
        fn was_cancelled(&self, _picker: &UIDocumentPickerViewController) {
            let _ = self.ivars().sender.try_send(Selection::Cancelled);
        }
    }
);

impl PickerDelegate {
    /// Anchors the waiting worker's sender in a main-thread-only delegate.
    fn new(marker: MainThreadMarker, sender: Sender<Selection>) -> Retained<Self> {
        let partial = marker.alloc().set_ivars(PickerState { sender });
        // SAFETY: the default NSObject init completes the allocated instance.
        unsafe { msg_send_id![super(partial), init] }
    }
}

/// Copies each picked document into private storage, preserving filenames and extensions.
fn copy_documents(
    urls: &[Retained<NSURL>],
    directory: &Path,
    filter: &Filter,
) -> Result<Vec<PickedFile>, String> {
    let mut picked = Vec::with_capacity(urls.len());
    for (index, url) in urls.iter().enumerate() {
        let name = display_name(url);
        // SAFETY: the scope closes as soon as this document is copied.
        let scoped = unsafe { url.startAccessingSecurityScopedResource() };
        let copied = copy_document(url, &directory.join(index.to_string()), &name, filter);
        if scoped {
            // SAFETY: closes the scope opened above.
            unsafe { url.stopAccessingSecurityScopedResource() };
        }
        // A failed copy removes this selection's copies before returning the error, like the
        // Android importer does.
        let path = copied.map_err(|error| {
            let _ = std::fs::remove_dir_all(directory);
            error
        })?;
        picked.push(PickedFile::copied(path));
    }
    Ok(picked)
}

/// Copies one document, refusing documents past the import limit before filling storage.
fn copy_document(
    url: &NSURL,
    directory: &Path,
    name: &str,
    filter: &Filter,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
    // SAFETY: reading the path of a document URL.
    let source = unsafe { url.path() }
        .ok_or_else(|| format!("Could not open {name}"))
        .map(|path| PathBuf::from(path.to_string()))?;
    if let Some(max_bytes) = filter.max_bytes {
        let size = std::fs::metadata(&source)
            .map_err(|error| format!("Could not open {name}: {error}"))?
            .len();
        if size > max_bytes {
            return Err(format!(
                "{name} is larger than the {} import limit",
                size_label(max_bytes)
            ));
        }
    }
    let target = directory.join(name);
    std::fs::copy(&source, &target).map_err(|error| format!("Could not copy {name}: {error}"))?;
    Ok(target)
}

/// The document's display name, kept as one path segment so its extension survives.
fn display_name(url: &NSURL) -> String {
    // SAFETY: reading a URL property.
    let name = unsafe { url.lastPathComponent() }.map_or_else(String::new, |name| name.to_string());
    let name = name.replace(['/', '\0'], "_");
    if name.is_empty() || name == "." || name == ".." {
        "import".to_owned()
    } else {
        name
    }
}

/// Formats a byte limit for the import status line, as the Android picker does.
fn size_label(bytes: u64) -> String {
    let mebibyte = 1024 * 1024;
    if bytes >= mebibyte && bytes % mebibyte == 0 {
        format!("{} MiB", bytes / mebibyte)
    } else {
        format!("{bytes}-byte")
    }
}
