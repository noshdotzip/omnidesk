//! PipeWire video capture — turning a ScreenCast grant into frames.
//!
//! [`crate::screen_cast`] gets as far as a node id and an authorised file descriptor.
//! This module connects to that node and receives the actual buffers.
//!
//! # Performance intent
//! The format list is ordered so the compositor's cheapest path wins. `BGRx`/`BGRA`
//! come first because that is what KWin composites in natively on Mesa — asking for
//! `RGBx` first would force a conversion on the compositor side, on every frame, for
//! nothing.
//!
//! Buffers may arrive as DMA-BUF file descriptors rather than mapped memory. Those are
//! GPU handles that can be imported straight into a hardware encoder with no CPU copy,
//! which is the whole point of this path. [`CaptureReport`] counts both kinds so the
//! caller can tell when the fast path silently was not taken.

use thiserror::Error;

#[cfg(target_os = "linux")]
use std::os::fd::OwnedFd;

#[derive(Debug, Error)]
pub enum CaptureError {
    #[error("PipeWire capture is only available on Linux builds")]
    Unsupported,
    #[error("PipeWire capture failed: {0}")]
    PipeWire(String),
}

/// How the compositor delivered a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A GPU buffer handle. Importable into a hardware encoder with no CPU copy.
    DmaBuf,
    /// CPU-visible memory. Correct, but every frame costs a copy and a GPU readback.
    MemPtr,
    /// Something else the compositor offered.
    Other,
}

/// What a capture run observed.
#[derive(Debug, Clone, Copy, Default)]
pub struct CaptureReport {
    pub frames: u64,
    pub width: u32,
    pub height: u32,
    pub max_framerate: u32,
    pub dma_buf_frames: u64,
    pub mem_ptr_frames: u64,
}

impl CaptureReport {
    /// Whether the zero-copy path was actually taken.
    ///
    /// Worth checking explicitly: a negotiation can succeed and still hand back mapped
    /// memory, which works but pays a GPU readback per frame. Silently accepting that
    /// is how a "hardware" pipeline ends up CPU-bound.
    pub fn used_zero_copy(&self) -> bool {
        self.dma_buf_frames > 0 && self.mem_ptr_frames == 0
    }
}

/// Connect to a granted node and receive frames until `frame_limit` or `timeout`.
///
/// Linux-only by signature rather than by runtime error: it takes an `OwnedFd`, a type
/// that does not exist on Windows, and there is no meaningful non-Linux behaviour to
/// stub. The pure reporting types above stay available everywhere so they can be tested
/// on both platforms.
#[cfg(target_os = "linux")]
pub fn capture(
    fd: OwnedFd,
    node_id: u32,
    frame_limit: u64,
    timeout: std::time::Duration,
) -> Result<CaptureReport, CaptureError> {
    imp::capture(fd, node_id, frame_limit, timeout)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use pipewire as pw;
    use pw::spa;
    use spa::pod::serialize::PodSerializer;
    use spa::pod::{Pod, Value};
    use std::cell::RefCell;
    use std::rc::Rc;

    fn pwerr(e: impl std::fmt::Display) -> CaptureError {
        CaptureError::PipeWire(e.to_string())
    }

    pub fn capture(
        fd: OwnedFd,
        node_id: u32,
        frame_limit: u64,
        timeout: std::time::Duration,
    ) -> Result<CaptureReport, CaptureError> {
        pw::init();

        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(pwerr)?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(pwerr)?;
        // Connecting by fd, not socket path: the portal already authorised this
        // connection, so we never need access to the PipeWire socket itself.
        let core = context.connect_fd_rc(fd, None).map_err(pwerr)?;

        let report = Rc::new(RefCell::new(CaptureReport::default()));

        let stream = pw::stream::StreamRc::new(
            core.clone(),
            "ultidesk-capture",
            pw::properties::properties! {
                *pw::keys::MEDIA_TYPE => "Video",
                *pw::keys::MEDIA_CATEGORY => "Capture",
                *pw::keys::MEDIA_ROLE => "Screen",
            },
        )
        .map_err(pwerr)?;

        let param_report = report.clone();
        let process_report = report.clone();
        let quit_loop = mainloop.clone();

        let _listener = stream
            .add_local_listener_with_user_data(())
            .param_changed(move |_, _, id, param| {
                let Some(param) = param else { return };
                if id != spa::param::ParamType::Format.as_raw() {
                    return;
                }
                let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param)
                else {
                    return;
                };
                if media_type != spa::param::format::MediaType::Video
                    || media_subtype != spa::param::format::MediaSubtype::Raw
                {
                    return;
                }
                let mut info = spa::param::video::VideoInfoRaw::default();
                if info.parse(param).is_ok() {
                    let mut r = param_report.borrow_mut();
                    r.width = info.size().width;
                    r.height = info.size().height;
                    r.max_framerate = info.max_framerate().num;
                }
            })
            .process(move |stream, _| {
                let Some(mut buffer) = stream.dequeue_buffer() else {
                    return;
                };
                let datas = buffer.datas_mut();
                let Some(first) = datas.first() else { return };

                let kind = match first.type_() {
                    spa::buffer::DataType::DmaBuf => FrameKind::DmaBuf,
                    spa::buffer::DataType::MemPtr | spa::buffer::DataType::MemFd => {
                        FrameKind::MemPtr
                    }
                    _ => FrameKind::Other,
                };

                let mut r = process_report.borrow_mut();
                r.frames += 1;
                match kind {
                    FrameKind::DmaBuf => r.dma_buf_frames += 1,
                    FrameKind::MemPtr => r.mem_ptr_frames += 1,
                    FrameKind::Other => {}
                }
                if r.frames >= frame_limit {
                    quit_loop.quit();
                }
            })
            .register()
            .map_err(pwerr)?;

        // Format preference order matters: BGRx/BGRA first because that is what KWin
        // composites in natively, so anything else costs a conversion per frame.
        let obj = pw::spa::pod::object!(
            spa::utils::SpaTypes::ObjectParamFormat,
            spa::param::ParamType::EnumFormat,
            pw::spa::pod::property!(
                spa::param::format::FormatProperties::MediaType,
                Id,
                spa::param::format::MediaType::Video
            ),
            pw::spa::pod::property!(
                spa::param::format::FormatProperties::MediaSubtype,
                Id,
                spa::param::format::MediaSubtype::Raw
            ),
            pw::spa::pod::property!(
                spa::param::format::FormatProperties::VideoFormat,
                Choice,
                Enum,
                Id,
                spa::param::video::VideoFormat::BGRx,
                spa::param::video::VideoFormat::BGRx,
                spa::param::video::VideoFormat::BGRA,
                spa::param::video::VideoFormat::RGBx,
                spa::param::video::VideoFormat::RGBA
            ),
            pw::spa::pod::property!(
                spa::param::format::FormatProperties::VideoSize,
                Choice,
                Range,
                Rectangle,
                spa::utils::Rectangle {
                    width: 1920,
                    height: 1080
                },
                spa::utils::Rectangle {
                    width: 1,
                    height: 1
                },
                spa::utils::Rectangle {
                    width: 8192,
                    height: 8192
                }
            ),
            pw::spa::pod::property!(
                spa::param::format::FormatProperties::VideoFramerate,
                Choice,
                Range,
                Fraction,
                spa::utils::Fraction { num: 60, denom: 1 },
                spa::utils::Fraction { num: 0, denom: 1 },
                spa::utils::Fraction { num: 240, denom: 1 }
            ),
        );

        let values: Vec<u8> =
            PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
                .map_err(pwerr)?
                .0
                .into_inner();
        let mut params = [Pod::from_bytes(&values)
            .ok_or_else(|| CaptureError::PipeWire("could not build the format pod".into()))?];

        stream
            .connect(
                spa::utils::Direction::Input,
                Some(node_id),
                // MAP_BUFFERS is deliberately NOT set. It forces PipeWire to mmap every
                // buffer, which defeats DMA-BUF: the whole point is to receive a GPU
                // handle and hand it straight to a hardware encoder. Mapping would add
                // a readback per frame, exactly the cost this path exists to avoid.
                pw::stream::StreamFlags::AUTOCONNECT,
                &mut params,
            )
            .map_err(pwerr)?;

        // A capture that never receives a frame must not hang the caller forever.
        let timeout_loop = mainloop.clone();
        let timer = mainloop.loop_().add_timer(move |_| timeout_loop.quit());
        let _ = timer.update_timer(Some(timeout), None);

        mainloop.run();

        let out = *report.borrow();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_copy_requires_dma_buf_and_nothing_else() {
        // The failure this guards: a negotiation that succeeds but hands back mapped
        // memory still "works", while paying a GPU readback per frame. Reporting that
        // as zero-copy is how a hardware pipeline ends up quietly CPU-bound.
        let all_dma = CaptureReport {
            frames: 10,
            dma_buf_frames: 10,
            ..Default::default()
        };
        assert!(all_dma.used_zero_copy());

        let mixed = CaptureReport {
            frames: 10,
            dma_buf_frames: 9,
            mem_ptr_frames: 1,
            ..Default::default()
        };
        assert!(
            !mixed.used_zero_copy(),
            "one mapped frame breaks the fast path"
        );

        let none = CaptureReport {
            frames: 10,
            mem_ptr_frames: 10,
            ..Default::default()
        };
        assert!(!none.used_zero_copy());
    }

    #[test]
    fn a_capture_that_saw_nothing_is_not_zero_copy() {
        assert!(!CaptureReport::default().used_zero_copy());
    }
}
