//! macOS screen capture with ScreenCaptureKit: one running stream that keeps the newest picture, instead of a
//! `screencapture` process per frame. Apple removed the older call (`CGDisplayCreateImage`) in macOS 26.

use std::io;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType,
};

use super::capture::Frame;

/// How long the system may take to answer (list the displays, start the stream, deliver the first picture).
const ANSWER: Duration = Duration::from_secs(3);
/// 'BGRA', which is the host's own pixel order.
const BGRA: u32 = u32::from_be_bytes(*b"BGRA");

type Latest = Arc<Mutex<Option<Frame>>>;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
}

/// Lets an Objective-C object cross to the thread that waits for it. ScreenCaptureKit objects are safe to use
/// from any thread; the bindings just cannot know it.
struct Sendable<T>(T);
unsafe impl<T> Send for Sendable<T> {}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "LucarnaScreenOutput"]
    #[ivars = Latest]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            // Idle frames (nothing changed) carry no picture.
            let Some(buffer) = (unsafe { sample.image_buffer() }) else {
                return;
            };
            let read_only = CVPixelBufferLockFlags::ReadOnly;
            if unsafe { CVPixelBufferLockBaseAddress(&buffer, read_only) } != 0 {
                return;
            }
            let (w, h) = (
                CVPixelBufferGetWidth(&buffer),
                CVPixelBufferGetHeight(&buffer),
            );
            let stride = CVPixelBufferGetBytesPerRow(&buffer);
            let base = CVPixelBufferGetBaseAddress(&buffer).cast::<u8>();
            let frame =
                (!base.is_null() && stride >= w * 4 && w <= 16384 && h <= 16384).then(|| {
                    let mut pixels = Vec::with_capacity(w * h * 4);
                    for row in 0..h {
                        // Rows can be padded: copy only the pixels of each one.
                        let line =
                            unsafe { std::slice::from_raw_parts(base.add(row * stride), w * 4) };
                        pixels.extend_from_slice(line);
                    }
                    Frame::new(w as u16, h as u16, pixels).ok()
                });
            unsafe { CVPixelBufferUnlockBaseAddress(&buffer, read_only) };
            if let Some(Some(frame)) = frame {
                *self.ivars().lock().unwrap() = Some(frame);
            }
        }
    }
);

/// A running capture of the main display. Dropping it stops the stream (and the system's recording indicator).
pub struct ScreenStream {
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    latest: Latest,
}

// The stream is only started, read and stopped; all three are thread-safe in ScreenCaptureKit.
unsafe impl Send for ScreenStream {}

fn failed(what: &str) -> io::Error {
    io::Error::other(format!("screen capture: {what}"))
}

impl ScreenStream {
    pub fn start() -> io::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let found = RcBlock::new(move |content: *mut SCShareableContent, _: *mut NSError| {
            // The main display, which is the one the pointer coordinates of `os_input` refer to.
            let main = unsafe { CGMainDisplayID() };
            let display = unsafe { content.as_ref() }.and_then(|c| {
                let all = unsafe { c.displays() };
                all.iter()
                    .find(|d| unsafe { d.displayID() } == main)
                    .or_else(|| all.firstObject())
            });
            let _ = tx.send(Sendable(display));
        });
        unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&found) };
        let display: Retained<SCDisplay> = rx
            .recv_timeout(ANSWER)
            .ok()
            .and_then(|d| d.0)
            .ok_or_else(|| failed("no display, or no Screen Recording permission"))?;

        let latest = Latest::default();
        let output: Retained<Output> = {
            let this = Output::alloc().set_ivars(Arc::clone(&latest));
            unsafe { msg_send![super(this), init] }
        };
        let stream = unsafe {
            let filter = SCContentFilter::initWithDisplay_excludingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
            );
            let config = SCStreamConfiguration::new();
            // The display's size in points: a Retina screen is sent at half its pixels, as before.
            config.setWidth(display.width() as usize);
            config.setHeight(display.height() as usize);
            config.setMinimumFrameInterval(CMTime::new(1, 60));
            config.setPixelFormat(BGRA);
            config.setShowsCursor(false);
            config.setQueueDepth(3);
            let stream = SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            );
            stream
                .addStreamOutput_type_sampleHandlerQueue_error(
                    ProtocolObject::from_ref(&*output),
                    SCStreamOutputType::Screen,
                    None,
                )
                .map_err(|_| failed("the stream refused its output"))?;
            stream
        };

        let (tx, rx) = mpsc::channel();
        let started = RcBlock::new(move |error: *mut NSError| {
            let _ = tx.send(error.is_null());
        });
        unsafe { stream.startCaptureWithCompletionHandler(Some(&started)) };
        if rx.recv_timeout(ANSWER) != Ok(true) {
            return Err(failed("the stream did not start"));
        }
        Ok(Self {
            stream,
            _output: output,
            latest,
        })
    }

    /// The newest picture; waits for the first one after the stream starts.
    pub fn latest(&self) -> io::Result<Frame> {
        let deadline = std::time::Instant::now() + ANSWER;
        loop {
            if let Some(frame) = self.latest.lock().unwrap().clone() {
                return Ok(frame);
            }
            if std::time::Instant::now() > deadline {
                return Err(failed("no picture arrived"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for ScreenStream {
    fn drop(&mut self) {
        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}
