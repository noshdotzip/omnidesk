//! PipeWire video capture — turning a ScreenCast grant into frames.
//!
//! [`crate::screen_cast`] gets as far as node ids and an authorised file descriptor.
//! This module connects to those nodes and receives the actual buffers.
//!
//! # One stream per window, never a cropped screen
//! A grant may cover several windows, and each arrives as its **own** PipeWire node.
//! They are captured concurrently on one connection, one stream each.
//!
//! This matters for correctness, not tidiness. The alternative — capturing the monitor
//! once and cropping each window out of it — cannot work: a monitor capture is the
//! *composited* result, so a window sitting behind another is simply not in the frame
//! and its proxy would show whatever is on top of it. Per-window capture asks the
//! compositor to render each window separately, which is correct whether the window is
//! covered, partly offscreen, or on another virtual desktop.
//!
//! # Performance intent
//! The format list is ordered so the compositor's cheapest path wins. `BGRx`/`BGRA`
//! come first because that is what KWin composites in natively on Mesa — asking for
//! `RGBx` first would force a conversion on the compositor side, on every frame, for
//! nothing.
//!
//! Buffers may arrive as DMA-BUF file descriptors rather than mapped memory. Those are
//! GPU handles that can be imported straight into a hardware encoder with no CPU copy.
//! [`CaptureReport`] counts both kinds so the caller can tell when the fast path
//! silently was not taken.

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

/// What one node produced during a capture run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureReport {
    pub node_id: u32,
    pub frames: u64,
    /// Buffer kind the compositor allocated, known before any frame arrives.
    ///
    /// Reported separately from the per-frame counts because a window that never
    /// changes produces no frames at all, and the negotiated buffer type is still the
    /// thing worth knowing. Waiting for a frame to learn it means a static window looks
    /// identical to a failed negotiation.
    pub allocated: Option<FrameKind>,
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

    /// Whether the compositor allocated GPU-backed buffers, regardless of frame count.
    ///
    /// This is the honest answer to "did zero copy negotiate?", because it does not
    /// depend on anything moving on screen.
    pub fn negotiated_dma_buf(&self) -> bool {
        self.allocated == Some(FrameKind::DmaBuf)
    }

    /// Whether this node produced anything at all.
    ///
    /// A window that never changes produces no frames: compositors send on damage, not
    /// on a clock. Zero frames therefore means "nothing moved", not "capture failed",
    /// and the two must not be conflated when reporting.
    pub fn saw_frames(&self) -> bool {
        self.frames > 0
    }
}

/// Capture every granted node concurrently on one connection.
///
/// Runs until every node has produced `frame_limit` frames, or `timeout` elapses —
/// whichever comes first. A static window may legitimately produce nothing.
///
/// Linux-only by signature rather than by runtime error: it takes an `OwnedFd`, a type
/// that does not exist on Windows. The pure reporting types above stay available
/// everywhere so they can be tested on both platforms.
#[cfg(target_os = "linux")]
pub fn capture_nodes(
    fd: OwnedFd,
    node_ids: &[u32],
    frame_limit: u64,
    timeout: std::time::Duration,
) -> Result<Vec<CaptureReport>, CaptureError> {
    imp::capture_nodes(fd, node_ids, frame_limit, timeout)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::*;
    use libspa_sys;
    use pipewire as pw;
    use pw::spa;
    use spa::pod::serialize::PodSerializer;
    use spa::pod::{Pod, Value};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn pwerr(e: impl std::fmt::Display) -> CaptureError {
        CaptureError::PipeWire(e.to_string())
    }

    type Reports = Rc<RefCell<HashMap<u32, CaptureReport>>>;

    /// Advertise which buffer kinds we accept, DMA-BUF first.
    ///
    /// This is the part that actually unlocks zero copy. Clearing
    /// `StreamFlags::MAP_BUFFERS` stops PipeWire mapping buffers for us, but on its own
    /// it changes nothing: without a `SPA_PARAM_Buffers` `dataType` mask naming
    /// `SPA_DATA_DmaBuf`, the compositor has no reason to offer GPU handles and falls
    /// back to shared memory. Measured on KWin: dropping MAP_BUFFERS alone still yielded
    /// mapped memory on every frame.
    ///
    /// MemFd and MemPtr stay in the mask as a fallback. A capture that refuses to run at
    /// all on a machine without DMA-BUF would be worse than one that runs slower.
    fn buffers_pod() -> Result<Vec<u8>, CaptureError> {
        // A single combined bitmask, not a list of alternatives. The C idiom is
        // SPA_POD_CHOICE_FLAGS_Int(mask) — one value with the acceptable bits OR-ed
        // together. Encoding it as enumerated alternatives makes PipeWire reject the
        // whole param with "error alloc buffers: Invalid argument", which surfaces as a
        // stream that negotiates a format and then never allocates a buffer.
        let mask = (1i32 << libspa_sys::SPA_DATA_DmaBuf)
            | (1i32 << libspa_sys::SPA_DATA_MemFd)
            | (1i32 << libspa_sys::SPA_DATA_MemPtr);
        let obj = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamBuffers.as_raw(),
            id: spa::param::ParamType::Buffers.as_raw(),
            properties: vec![spa::pod::Property {
                key: libspa_sys::SPA_PARAM_BUFFERS_dataType,
                flags: spa::pod::PropertyFlags::empty(),
                value: Value::Choice(spa::pod::ChoiceValue::Int(spa::utils::Choice(
                    spa::utils::ChoiceFlags::empty(),
                    spa::utils::ChoiceEnum::Flags {
                        default: mask,
                        flags: vec![mask],
                    },
                ))),
            }],
        };
        Ok(
            PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
                .map_err(pwerr)?
                .0
                .into_inner(),
        )
    }

    /// `DRM_FORMAT_MOD_INVALID` — "whatever the driver picks implicitly".
    ///
    /// Offering this rather than an enumerated modifier list avoids having to stand up
    /// EGL/GBM just to ask the GPU what it supports. The compositor is free to answer
    /// with its own modifier because the property is DONT_FIXATE.
    const DRM_FORMAT_MOD_INVALID: i64 = 0x00ff_ffff_ffff_ffff;
    const DRM_FORMAT_MOD_LINEAR: i64 = 0;

    fn id_choice(default: u32, alternatives: Vec<u32>) -> Value {
        Value::Choice(spa::pod::ChoiceValue::Id(spa::utils::Choice(
            spa::utils::ChoiceFlags::empty(),
            spa::utils::ChoiceEnum::Enum {
                default: spa::utils::Id(default),
                alternatives: alternatives.into_iter().map(spa::utils::Id).collect(),
            },
        )))
    }

    fn size_choice() -> Value {
        Value::Choice(spa::pod::ChoiceValue::Rectangle(spa::utils::Choice(
            spa::utils::ChoiceFlags::empty(),
            spa::utils::ChoiceEnum::Range {
                default: spa::utils::Rectangle {
                    width: 1920,
                    height: 1080,
                },
                min: spa::utils::Rectangle {
                    width: 1,
                    height: 1,
                },
                max: spa::utils::Rectangle {
                    width: 8192,
                    height: 8192,
                },
            },
        )))
    }

    fn framerate_choice() -> Value {
        Value::Choice(spa::pod::ChoiceValue::Fraction(spa::utils::Choice(
            spa::utils::ChoiceFlags::empty(),
            spa::utils::ChoiceEnum::Range {
                default: spa::utils::Fraction { num: 60, denom: 1 },
                min: spa::utils::Fraction { num: 0, denom: 1 },
                max: spa::utils::Fraction { num: 240, denom: 1 },
            },
        )))
    }

    /// An EnumFormat that also carries a DRM modifier, which is what makes a
    /// compositor willing to hand back DMA-BUF.
    ///
    /// Advertising `SPA_PARAM_BUFFERS_dataType` is necessary but not sufficient:
    /// measured against KWin, a format with no modifier property allocates shared
    /// memory every time. The modifier must be MANDATORY (the compositor may not
    /// silently drop it) and DONT_FIXATE (we accept whichever modifier it chooses).
    ///
    /// Offered *alongside* the plain format rather than instead of it, so a compositor
    /// or GPU that cannot do DMA-BUF still negotiates the shared-memory path instead
    /// of failing outright. Slower is acceptable; not working is not.
    fn format_pod_with_modifier() -> Result<Vec<u8>, CaptureError> {
        use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
        use spa::param::video::VideoFormat;
        use spa::pod::{Property, PropertyFlags};

        let obj = spa::pod::Object {
            type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: spa::param::ParamType::EnumFormat.as_raw(),
            properties: vec![
                Property::new(
                    FormatProperties::MediaType.as_raw(),
                    Value::Id(spa::utils::Id(MediaType::Video.as_raw())),
                ),
                Property::new(
                    FormatProperties::MediaSubtype.as_raw(),
                    Value::Id(spa::utils::Id(MediaSubtype::Raw.as_raw())),
                ),
                Property::new(
                    FormatProperties::VideoFormat.as_raw(),
                    id_choice(
                        VideoFormat::BGRx.as_raw(),
                        vec![
                            VideoFormat::BGRx.as_raw(),
                            VideoFormat::BGRA.as_raw(),
                            VideoFormat::RGBx.as_raw(),
                            VideoFormat::RGBA.as_raw(),
                        ],
                    ),
                ),
                Property {
                    key: FormatProperties::VideoModifier.as_raw(),
                    flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
                    value: Value::Choice(spa::pod::ChoiceValue::Long(spa::utils::Choice(
                        spa::utils::ChoiceFlags::empty(),
                        spa::utils::ChoiceEnum::Enum {
                            default: DRM_FORMAT_MOD_INVALID,
                            alternatives: vec![DRM_FORMAT_MOD_INVALID, DRM_FORMAT_MOD_LINEAR],
                        },
                    ))),
                },
                Property::new(FormatProperties::VideoSize.as_raw(), size_choice()),
                Property::new(
                    FormatProperties::VideoFramerate.as_raw(),
                    framerate_choice(),
                ),
            ],
        };
        Ok(
            PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
                .map_err(pwerr)?
                .0
                .into_inner(),
        )
    }

    /// The EnumFormat pod every stream offers.
    fn format_pod() -> Result<Vec<u8>, CaptureError> {
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
        Ok(
            PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
                .map_err(pwerr)?
                .0
                .into_inner(),
        )
    }

    pub fn capture_nodes(
        fd: OwnedFd,
        node_ids: &[u32],
        frame_limit: u64,
        timeout: std::time::Duration,
    ) -> Result<Vec<CaptureReport>, CaptureError> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        pw::init();

        let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(pwerr)?;
        let context = pw::context::ContextRc::new(&mainloop, None).map_err(pwerr)?;
        // Connecting by fd, not socket path: the portal already authorised this
        // connection, so we never need access to the PipeWire socket itself.
        let core = context.connect_fd_rc(fd, None).map_err(pwerr)?;

        let reports: Reports = Rc::new(RefCell::new(
            node_ids
                .iter()
                .map(|&id| {
                    (
                        id,
                        CaptureReport {
                            node_id: id,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        ));

        let total_nodes = node_ids.len();
        // Streams and listeners must outlive the loop; dropping either stops delivery.
        let mut streams = Vec::with_capacity(total_nodes);
        let mut listeners = Vec::with_capacity(total_nodes);

        for &node_id in node_ids {
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

            let param_reports = reports.clone();
            let process_reports = reports.clone();
            let quit_loop = mainloop.clone();

            let alloc_reports = reports.clone();

            let listener = stream
                // The node id rides along as user data, so one callback body serves
                // every stream without a closure per node capturing its own copy.
                .add_local_listener_with_user_data(node_id)
                .state_changed(move |_, &mut node_id, old, new| {
                    // A stream that negotiates a format but never allocates buffers is
                    // stuck somewhere in this transition, and the state is the only
                    // thing that says where.
                    tracing::info!(node = node_id, ?old, ?new, "stream state");
                })
                .add_buffer(move |_, &mut node_id, buffer| {
                    // Fires when buffers are allocated, before any frame. This is what
                    // makes the zero-copy question answerable on a window that never
                    // changes.
                    // SAFETY: PipeWire hands us a live pw_buffer for the duration of
                    // this callback; we only read the first data block's type.
                    let kind = unsafe {
                        let b = (*buffer).buffer;
                        if b.is_null() || (*b).n_datas == 0 {
                            return;
                        }
                        match (*(*b).datas).type_ {
                            t if t == libspa_sys::SPA_DATA_DmaBuf => FrameKind::DmaBuf,
                            t if t == libspa_sys::SPA_DATA_MemFd
                                || t == libspa_sys::SPA_DATA_MemPtr =>
                            {
                                FrameKind::MemPtr
                            }
                            _ => FrameKind::Other,
                        }
                    };
                    if let Some(r) = alloc_reports.borrow_mut().get_mut(&node_id) {
                        r.allocated = Some(kind);
                    }
                })
                .param_changed(move |stream, &mut node_id, id, param| {
                    let Some(param) = param else { return };
                    if id != spa::param::ParamType::Format.as_raw() {
                        return;
                    }
                    let Ok((media_type, media_subtype)) =
                        spa::param::format_utils::parse_format(param)
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
                        if let Some(r) = param_reports.borrow_mut().get_mut(&node_id) {
                            r.width = info.size().width;
                            r.height = info.size().height;
                            r.max_framerate = info.max_framerate().num;
                        }
                    }

                    // Format is settled; now say which buffer kinds we take. This has
                    // to happen here rather than at connect() time, because the buffer
                    // parameters are only meaningful once the format is known.
                    if let Ok(values) = buffers_pod() {
                        if let Some(pod) = spa::pod::Pod::from_bytes(&values) {
                            if let Err(e) = stream.update_params(&mut [pod]) {
                                tracing::warn!(error = %e, "could not advertise DMA-BUF support");
                            }
                        }
                    }
                })
                .process(move |stream, &mut node_id| {
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

                    let mut map = process_reports.borrow_mut();
                    if let Some(r) = map.get_mut(&node_id) {
                        r.frames += 1;
                        match kind {
                            FrameKind::DmaBuf => r.dma_buf_frames += 1,
                            FrameKind::MemPtr => r.mem_ptr_frames += 1,
                            FrameKind::Other => {}
                        }
                    }
                    // Stop only when every node has had its fill, so a busy window
                    // cannot end the run before a quiet one has been observed.
                    if map.values().all(|r| r.frames >= frame_limit) {
                        quit_loop.quit();
                    }
                })
                .register()
                .map_err(pwerr)?;

            // Order matters: the compositor takes the first format it can satisfy, so
            // the DMA-BUF-capable one goes first and the plain one is the fallback.
            let with_mod = format_pod_with_modifier()?;
            let plain = format_pod()?;
            let mut params = [
                Pod::from_bytes(&with_mod).ok_or_else(|| {
                    CaptureError::PipeWire("could not build the modifier format pod".into())
                })?,
                Pod::from_bytes(&plain).ok_or_else(|| {
                    CaptureError::PipeWire("could not build the format pod".into())
                })?,
            ];

            stream
                .connect(
                    spa::utils::Direction::Input,
                    Some(node_id),
                    // MAP_BUFFERS is deliberately NOT set. It forces PipeWire to mmap
                    // every buffer, which defeats DMA-BUF: the point of that path is to
                    // receive a GPU handle and hand it to a hardware encoder without a
                    // readback.
                    pw::stream::StreamFlags::AUTOCONNECT,
                    &mut params,
                )
                .map_err(pwerr)?;

            streams.push(stream);
            listeners.push(listener);
        }

        // A capture where nothing moves must not hang the caller forever.
        let timeout_loop = mainloop.clone();
        let timer = mainloop.loop_().add_timer(move |_| timeout_loop.quit());
        let _ = timer.update_timer(Some(timeout), None);

        mainloop.run();

        let map = reports.borrow();
        let mut out: Vec<CaptureReport> = node_ids
            .iter()
            .filter_map(|id| map.get(id).copied())
            .collect();
        out.sort_by_key(|r| r.node_id);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(dma: u64, mem: u64) -> CaptureReport {
        CaptureReport {
            node_id: 1,
            frames: dma + mem,
            dma_buf_frames: dma,
            mem_ptr_frames: mem,
            ..Default::default()
        }
    }

    #[test]
    fn zero_copy_requires_dma_buf_and_nothing_else() {
        // The failure this guards: a negotiation that succeeds but hands back mapped
        // memory still "works", while paying a GPU readback per frame. Reporting that
        // as zero-copy is how a hardware pipeline ends up quietly CPU-bound.
        assert!(report(10, 0).used_zero_copy());
        assert!(
            !report(9, 1).used_zero_copy(),
            "one mapped frame breaks the fast path"
        );
        assert!(!report(0, 10).used_zero_copy());
    }

    #[test]
    fn a_capture_that_saw_nothing_is_not_zero_copy() {
        assert!(!CaptureReport::default().used_zero_copy());
    }

    #[test]
    fn no_frames_is_reported_separately_from_no_zero_copy() {
        // A static window legitimately produces nothing, because compositors send on
        // damage rather than on a clock. Conflating "nothing moved" with "capture
        // failed" would send someone debugging a working pipeline.
        let idle = CaptureReport::default();
        assert!(!idle.saw_frames());

        let busy = report(0, 30);
        assert!(busy.saw_frames());
        assert!(!busy.used_zero_copy());
    }
}
