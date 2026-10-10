//! Terminal graphics for the preview pane (PLAN §8: `ratatui-image`).
//!
//! The decoded image comes from the preview engine (`Arc<DynamicImage>`, already downscaled
//! to at most 2048 px). Turning it into Sixel / halfblock cells at the size of the pane is
//! the expensive part, so it runs on a worker thread (`ThreadProtocol`) and the UI draws
//! whatever is ready; when the worker finishes it calls the notifier, which the application
//! turns into a redraw message.
//!
//! The protocol state is process-global because `render(frame, state, theme)` takes the
//! state by shared reference and the protocol objects must be mutated while drawing. It is
//! keyed by the opaque `PreviewState::image_id` of `ks-core`, never stored in `AppState`.
//! Without an installed host (tests, or a terminal where detection was skipped) a
//! synchronous halfblocks host is created on first use, so rendering is deterministic.

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use image::DynamicImage;
use ks_core::config::ImageProtocol;
use ratatui::layout::{Rect, Size};
use ratatui::Frame;
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::thread::{ResizeRequest, ThreadProtocol};
use ratatui_image::{FilterType, Resize, ResizeEncodeRender};

/// How long start-up waits for the terminal to answer the capability query. Windows
/// Terminal answers within a few milliseconds; a terminal that does not answer at all costs
/// this much once, at start-up, and then gets halfblocks.
pub const QUERY_TIMEOUT: Duration = Duration::from_millis(250);

type Notifier = Arc<dyn Fn() + Send + Sync>;

struct Host {
    picker: Picker,
    proto: ThreadProtocol,
    /// `PreviewState::image_id` of the image inside `proto`.
    current: Option<u64>,
    /// Receiver of resize requests when no worker thread exists (synchronous mode).
    inline: Option<Receiver<ResizeRequest>>,
    /// Hands a newly current image to the prepare thread (None in synchronous mode, where the
    /// protocol is built while drawing).
    prepare: Option<mpsc::Sender<Prepare>>,
    /// `proto` holds the image `current` names (false while the prepare thread works).
    ready: bool,
    /// Area the image was drawn into by the previous frame, and by the current one.
    drawn_prev: Option<Rect>,
    drawn_now: Option<Rect>,
}

/// A new image for the prepare thread: cloning a 2048 px bitmap and building the resize
/// protocol is a few milliseconds that must not be spent on the UI thread.
struct Prepare {
    id: u64,
    image: Arc<DynamicImage>,
    picker: Picker,
}

static HOST: Mutex<Option<Host>> = Mutex::new(None);
static NOTIFIER: Mutex<Option<Notifier>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Asks the terminal what it can do and returns a picker for it.
///
/// Must run after the alternate screen is entered and before input is read (the query reads
/// the terminal's answer from stdin). `ImageProtocol::Halfblocks` skips the query. A forced
/// protocol still queries, for the font size, and then overrides the detected protocol.
pub fn detect_picker(preference: ImageProtocol) -> Picker {
    if preference == ImageProtocol::Halfblocks {
        return Picker::halfblocks();
    }
    let options = QueryStdioOptions {
        timeout: QUERY_TIMEOUT,
        ..QueryStdioOptions::default()
    };
    let mut picker =
        Picker::from_query_stdio_with_options(options).unwrap_or_else(|_| Picker::halfblocks());
    match preference {
        ImageProtocol::Auto | ImageProtocol::Halfblocks => {}
        ImageProtocol::Sixel => picker.set_protocol_type(ProtocolType::Sixel),
        ImageProtocol::Kitty => picker.set_protocol_type(ProtocolType::Kitty),
        ImageProtocol::Iterm2 => picker.set_protocol_type(ProtocolType::Iterm2),
    }
    picker
}

/// A short description for the log: `sixel 10x20` or `halfblocks 10x20`.
pub fn describe(picker: &Picker) -> String {
    let font = picker.font_size();
    let protocol = match picker.protocol_type() {
        ProtocolType::Halfblocks => "halfblocks",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Kitty => "kitty",
        ProtocolType::Iterm2 => "iterm2",
    };
    format!("{protocol} (cell {}x{} px)", font.width, font.height)
}

/// Installs the picker and starts the worker thread that resizes and encodes images.
pub fn install(picker: Picker) {
    let (tx, rx) = mpsc::channel::<ResizeRequest>();
    let (prepare_tx, prepare_rx) = mpsc::channel::<Prepare>();
    let host = Host {
        picker,
        proto: ThreadProtocol::new(tx, None),
        current: None,
        inline: None,
        prepare: Some(prepare_tx),
        ready: false,
        drawn_prev: None,
        drawn_now: None,
    };
    *lock(&HOST) = Some(host);
    thread::Builder::new()
        .name("ks-image-prepare".into())
        .spawn(move || {
            while let Ok(mut job) = prepare_rx.recv() {
                while let Ok(newer) = prepare_rx.try_recv() {
                    job = newer;
                }
                let protocol = job
                    .picker
                    .new_resize_protocol(DynamicImage::clone(&job.image));
                let mut guard = lock(&HOST);
                let Some(host) = guard.as_mut().filter(|h| h.current == Some(job.id)) else {
                    continue;
                };
                host.proto.replace_protocol(protocol);
                host.ready = true;
                drop(guard);
                if let Some(notify) = lock(&NOTIFIER).clone() {
                    notify();
                }
            }
        })
        .ok();
    thread::Builder::new()
        .name("ks-image-encode".into())
        .spawn(move || {
            while let Ok(request) = rx.recv() {
                // Newer requests make older ones pointless; skip to the latest.
                let mut request = request;
                while let Ok(newer) = rx.try_recv() {
                    request = newer;
                }
                let Ok(response) = request.resize_encode() else {
                    continue;
                };
                let updated = lock(&HOST)
                    .as_mut()
                    .is_some_and(|host| host.proto.update_resized_protocol(response));
                if updated {
                    if let Some(notify) = lock(&NOTIFIER).clone() {
                        notify();
                    }
                }
            }
        })
        .ok();
}

/// Registers the callback that asks the application to redraw when a background encode
/// finished.
pub fn set_notifier(notify: impl Fn() + Send + Sync + 'static) {
    *lock(&NOTIFIER) = Some(Arc::new(notify));
}

/// Marks the start of a frame (see [`finish_frame`]).
pub(crate) fn begin_frame() {
    if let Some(host) = lock(&HOST).as_mut() {
        host.drawn_prev = host.drawn_now.take();
    }
}

/// True when the frame that was just drawn left graphics on screen that the terminal will
/// not erase by itself (the image moved, shrank or disappeared). The caller clears the
/// terminal and redraws. Always false for halfblocks, which are ordinary cells.
pub fn finish_frame() -> bool {
    let mut guard = lock(&HOST);
    let Some(host) = guard.as_mut() else {
        return false;
    };
    if host.picker.protocol_type() == ProtocolType::Halfblocks {
        return false;
    }
    graphics_left(host.drawn_prev, host.drawn_now)
}

/// True when the previous frame had an image and this one has none or one elsewhere.
fn graphics_left(prev: Option<Rect>, now: Option<Rect>) -> bool {
    prev.is_some() && prev != now
}

/// Draws `image` (identified by `id`) into `area`, centred, keeping its aspect ratio.
///
/// The resize decision always uses the full `area` (that is what keeps `needs_resize` stable
/// between frames); only the drawing position is centred.
pub(crate) fn render_image(frame: &mut Frame, area: Rect, id: u64, image: &Arc<DynamicImage>) {
    let mut guard = lock(&HOST);
    let host = guard.get_or_insert_with(sync_host);
    if host.current != Some(id) {
        host.current = Some(id);
        if let Some(prepare) = &host.prepare {
            host.ready = false;
            let job = Prepare {
                id,
                image: Arc::clone(image),
                picker: host.picker.clone(),
            };
            let _ = prepare.send(job);
        } else {
            let protocol = host.picker.new_resize_protocol(DynamicImage::clone(image));
            host.proto.replace_protocol(protocol);
            host.ready = true;
        }
    }
    if !host.ready {
        return;
    }
    let resize = Resize::Fit(Some(FilterType::Triangle));
    let full = Size::new(area.width, area.height);
    if let Some(wanted) = host.proto.needs_resize(&resize, full) {
        host.proto.resize_encode(&resize, wanted);
    }
    // Synchronous mode: do the queued work now.
    if let Some(rx) = &host.inline {
        while let Ok(request) = rx.try_recv() {
            if let Ok(response) = request.resize_encode() {
                host.proto.update_resized_protocol(response);
            }
        }
    }
    // While a background encode is pending there is nothing to draw (`size_for` is None).
    if let Some(fit) = host.proto.size_for(resize, full) {
        let target = center(area, fit);
        host.drawn_now = Some(target);
        host.proto.render(target, frame.buffer_mut());
    }
}

fn center(area: Rect, size: Size) -> Rect {
    let width = size.width.min(area.width);
    let height = size.height.min(area.height);
    Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    )
}

/// A host without a worker thread: encodes inline while drawing. Used by tests and when
/// nothing was installed.
fn sync_host() -> Host {
    let (tx, rx) = mpsc::channel();
    Host {
        picker: Picker::halfblocks(),
        proto: ThreadProtocol::new(tx, None),
        current: None,
        inline: Some(rx),
        prepare: None,
        ready: false,
        drawn_prev: None,
        drawn_now: None,
    }
}

/// The host is process-global; tests that draw images run one at a time.
#[cfg(test)]
pub(crate) static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Test helper: forget the installed host so the next draw uses a fresh synchronous one.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    *lock(&HOST) = None;
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;

    #[test]
    fn centering_stays_inside_the_area() {
        let area = Rect::new(10, 5, 40, 20);
        assert_eq!(center(area, Size::new(10, 4)), Rect::new(25, 13, 10, 4));
        assert_eq!(center(area, Size::new(80, 40)), area);
    }

    #[test]
    fn graphics_that_left_the_screen_ask_for_a_clear() {
        let spot = Some(Rect::new(1, 1, 10, 5));
        assert!(
            !graphics_left(None, spot),
            "first draw: nothing to wipe yet"
        );
        assert!(!graphics_left(spot, spot), "same place: nothing to wipe");
        assert!(graphics_left(spot, None), "the image went away: wipe it");
        assert!(graphics_left(spot, Some(Rect::new(2, 1, 10, 5))), "moved");
        assert!(!graphics_left(None, None), "and only once");
    }

    #[test]
    fn halfblocks_render_synchronously_without_an_installed_host() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_tests();
        let image = Arc::new(crate::testutil_preview::two_tone(64, 64));
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        terminal
            .draw(|f| {
                begin_frame();
                render_image(f, f.area(), 1, &image);
            })
            .unwrap();

        let drawn = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "▀" || cell.symbol() == "▄");
        assert!(drawn, "expected halfblock cells");
        assert!(!finish_frame(), "halfblocks never need a terminal clear");
    }

    fn has_halfblocks(terminal: &Terminal<TestBackend>) -> bool {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .any(|cell| cell.symbol() == "▀" || cell.symbol() == "▄")
    }

    #[test]
    fn the_prepare_thread_builds_only_the_latest_image_and_notifies() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let _guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        reset_for_tests();
        install(Picker::halfblocks());
        let notified = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&notified);
        set_notifier(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let a = Arc::new(crate::testutil_preview::two_tone(64, 64));
        let b = Arc::new(crate::testutil_preview::two_tone(32, 48));
        let mut terminal = Terminal::new(TestBackend::new(30, 10)).unwrap();
        let draw = |terminal: &mut Terminal<TestBackend>, id: u64, image: &Arc<_>| {
            terminal
                .draw(|f| {
                    begin_frame();
                    render_image(f, f.area(), id, image);
                })
                .unwrap();
        };
        // Not ready yet: the first frame draws nothing instead of blocking on the clone.
        draw(&mut terminal, 1, &a);
        assert!(
            !has_halfblocks(&terminal),
            "the protocol is built off-thread"
        );
        // A newer image supersedes the first before it was applied.
        draw(&mut terminal, 2, &b);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !has_halfblocks(&terminal) {
            assert!(
                std::time::Instant::now() < deadline,
                "image never became ready"
            );
            thread::sleep(Duration::from_millis(10));
            draw(&mut terminal, 2, &b);
        }
        {
            let guard = lock(&HOST);
            let host = guard.as_ref().unwrap();
            assert_eq!(host.current, Some(2));
            assert!(host.ready);
        }
        assert!(
            notified.load(Ordering::SeqCst) >= 1,
            "the redraw was requested"
        );
        *lock(&NOTIFIER) = None;
        reset_for_tests();
    }
}
