//! WASAPI loopback capture — recording what this machine is *playing*.
//!
//! The Linux side gets this for free: PipeWire exposes a `.monitor` source for every
//! sink. Windows has no such node. Loopback capture means opening the **render**
//! endpoint with `AUDCLNT_STREAMFLAGS_LOOPBACK` and reading from it as if it were a
//! capture device, which `cpal` does not expose, so this talks to WASAPI directly.
//!
//! # The format is the device's choice, not ours
//! `GetMixFormat` returns whatever the endpoint is mixing at — typically 32-bit float,
//! 48 kHz, stereo. There is no asking for something else in shared mode, so the capture
//! reports what it actually got and the caller puts *that* in the stream header.
//! Hardcoding 48 kHz stereo s16 here would produce plausible-sounding garbage on any
//! machine that mixes at 44.1 kHz or in 5.1.
//!
//! # Silence is not nothing
//! An idle endpoint returns buffers flagged `AUDCLNT_BUFFERFLAGS_SILENT` whose contents
//! are undefined rather than zeroed. Those have to be emitted as real zeroes: passing
//! the undefined memory through would send noise whenever the machine is quiet.

use std::sync::mpsc::Receiver;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LoopbackError {
    #[error("WASAPI loopback capture is only available on Windows builds")]
    Unsupported,
    #[error("WASAPI loopback capture failed: {0}")]
    Wasapi(String),
}

/// A running loopback capture, plus the format the device actually gave us.
pub struct LoopbackStream {
    pub rate: u32,
    pub channels: u16,
    /// Interleaved `i16` samples, in device channel order.
    pub samples: Receiver<Vec<i16>>,
}

/// Convert a normalized float sample to `i16`.
///
/// Clamped before scaling: WASAPI float buffers are not guaranteed to stay inside
/// `[-1.0, 1.0]` (a chain with gain applied can exceed it), and letting an out-of-range
/// value wrap turns a loud passage into harsh noise instead of clipping.
pub fn f32_to_i16(sample: f32) -> i16 {
    if sample.is_nan() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

/// Start capturing the default render endpoint's loopback.
pub fn spawn_loopback_capture() -> Result<LoopbackStream, LoopbackError> {
    spawn_loopback_capture_on(None)
}

/// Start capturing a specific render endpoint's loopback, by endpoint id.
///
/// `None` means the default endpoint, and *follows* it: WASAPI resolves the default at
/// activation, so this is the right choice when the operator has not picked a device.
/// Passing an id from [`crate::audio_devices::enumerate`] pins the capture to that
/// endpoint instead.
///
/// The distinction matters as soon as a machine has more than one output: without it a
/// route that names the HDMI output would silently capture the speakers.
pub fn spawn_loopback_capture_on(
    endpoint_id: Option<&str>,
) -> Result<LoopbackStream, LoopbackError> {
    #[cfg(windows)]
    {
        imp::spawn_loopback_capture(endpoint_id.map(str::to_owned))
    }
    #[cfg(not(windows))]
    {
        let _ = endpoint_id;
        Err(LoopbackError::Unsupported)
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::sync::mpsc;
    use windows::Win32::Media::Audio::{
        eConsole, eRender, IAudioCaptureClient, IAudioClient, IMMDeviceEnumerator,
        MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED,
        AUDCLNT_STREAMFLAGS_LOOPBACK,
    };
    use windows::Win32::Media::KernelStreaming::WAVE_FORMAT_EXTENSIBLE;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED,
    };

    /// `WAVE_FORMAT_IEEE_FLOAT`. Defined here because windows-rs does not export it
    /// from `Win32::Media::Audio`; the value is fixed by the WAVE format spec.
    const WAVE_FORMAT_IEEE_FLOAT: u32 = 0x0003;

    /// 200ms buffer, in 100-nanosecond units, as WASAPI wants.
    const BUFFER_DURATION_100NS: i64 = 200 * 10_000;

    fn wasapi(e: impl std::fmt::Display) -> LoopbackError {
        LoopbackError::Wasapi(e.to_string())
    }

    pub fn spawn_loopback_capture(
        endpoint_id: Option<String>,
    ) -> Result<LoopbackStream, LoopbackError> {
        let (fmt_tx, fmt_rx) = mpsc::channel::<Result<(u32, u16), String>>();
        let (sample_tx, sample_rx) = mpsc::channel::<Vec<i16>>();

        std::thread::Builder::new()
            .name("ultidesk-wasapi-loopback".into())
            .spawn(move || {
                // COM must be initialized on the thread that uses these interfaces, and
                // the interfaces must not outlive it — hence everything staying here.
                // SAFETY: called once on this thread before any COM use.
                let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };

                let run = || -> Result<(), LoopbackError> {
                    // SAFETY: standard WASAPI activation sequence; each call is checked
                    // and every pointer handed out is released before returning.
                    unsafe {
                        let enumerator: IMMDeviceEnumerator =
                            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                                .map_err(wasapi)?;
                        // An explicit id pins the capture; no id follows the default,
                        // which is what an operator who has not chosen expects.
                        let device = match endpoint_id.as_deref() {
                            Some(id) => {
                                let wide: Vec<u16> =
                                    id.encode_utf16().chain(std::iter::once(0)).collect();
                                enumerator
                                    .GetDevice(windows::core::PCWSTR(wide.as_ptr()))
                                    .map_err(wasapi)?
                            }
                            None => enumerator
                                .GetDefaultAudioEndpoint(eRender, eConsole)
                                .map_err(wasapi)?,
                        };
                        let client: IAudioClient =
                            device.Activate(CLSCTX_ALL, None).map_err(wasapi)?;

                        let mix = client.GetMixFormat().map_err(wasapi)?;
                        let rate = (*mix).nSamplesPerSec;
                        let channels = (*mix).nChannels;
                        let bits = (*mix).wBitsPerSample;
                        let tag = (*mix).wFormatTag as u32;
                        // WAVE_FORMAT_EXTENSIBLE hides the real type in a sub-format
                        // GUID. Rather than parse it, infer from the sample width, which
                        // is what actually determines how to read the bytes.
                        let is_float = tag == WAVE_FORMAT_IEEE_FLOAT
                            || (tag == WAVE_FORMAT_EXTENSIBLE && bits == 32);

                        client
                            .Initialize(
                                AUDCLNT_SHAREMODE_SHARED,
                                AUDCLNT_STREAMFLAGS_LOOPBACK,
                                BUFFER_DURATION_100NS,
                                0,
                                mix,
                                None,
                            )
                            .map_err(wasapi)?;

                        let capture: IAudioCaptureClient = client.GetService().map_err(wasapi)?;
                        client.Start().map_err(wasapi)?;

                        if fmt_tx.send(Ok((rate, channels))).is_err() {
                            let _ = client.Stop();
                            return Ok(());
                        }

                        let frame_samples = channels as usize;
                        loop {
                            let mut available = capture.GetNextPacketSize().map_err(wasapi)?;
                            if available == 0 {
                                // Loopback yields nothing while the endpoint is idle, so
                                // poll rather than spin.
                                std::thread::sleep(std::time::Duration::from_millis(5));
                                continue;
                            }
                            while available > 0 {
                                let mut data: *mut u8 = std::ptr::null_mut();
                                let mut frames: u32 = 0;
                                let mut flags: u32 = 0;
                                capture
                                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                                    .map_err(wasapi)?;

                                let count = frames as usize * frame_samples;
                                let mut out = Vec::with_capacity(count);
                                if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 {
                                    // Contents are undefined, not zeroed. Emit real
                                    // silence instead of forwarding whatever was there.
                                    out.resize(count, 0i16);
                                } else if is_float {
                                    let src = std::slice::from_raw_parts(data as *const f32, count);
                                    out.extend(src.iter().copied().map(f32_to_i16));
                                } else if bits == 16 {
                                    let src = std::slice::from_raw_parts(data as *const i16, count);
                                    out.extend_from_slice(src);
                                } else {
                                    let _ = capture.ReleaseBuffer(frames);
                                    let _ = client.Stop();
                                    return Err(LoopbackError::Wasapi(format!(
                                        "unsupported mix format: {bits}-bit, tag {tag}"
                                    )));
                                }

                                capture.ReleaseBuffer(frames).map_err(wasapi)?;
                                if sample_tx.send(out).is_err() {
                                    let _ = client.Stop();
                                    return Ok(()); // consumer gone
                                }
                                available = capture.GetNextPacketSize().map_err(wasapi)?;
                            }
                        }
                    }
                };

                if let Err(e) = run() {
                    let _ = fmt_tx.send(Err(e.to_string()));
                }
            })
            .map_err(wasapi)?;

        match fmt_rx.recv() {
            Ok(Ok((rate, channels))) => Ok(LoopbackStream {
                rate,
                channels,
                samples: sample_rx,
            }),
            Ok(Err(e)) => Err(LoopbackError::Wasapi(e)),
            Err(e) => Err(LoopbackError::Wasapi(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_conversion_hits_the_expected_endpoints() {
        assert_eq!(f32_to_i16(0.0), 0);
        assert_eq!(f32_to_i16(1.0), i16::MAX);
        assert_eq!(f32_to_i16(-1.0), -i16::MAX);
    }

    #[test]
    fn out_of_range_samples_clip_rather_than_wrap() {
        // A chain with gain applied can exceed 1.0. Wrapping would turn a loud passage
        // into harsh noise; clipping is merely loud.
        assert_eq!(f32_to_i16(2.5), i16::MAX);
        assert_eq!(f32_to_i16(-2.5), -i16::MAX);
        assert_eq!(f32_to_i16(f32::INFINITY), i16::MAX);
        assert_eq!(f32_to_i16(f32::NEG_INFINITY), -i16::MAX);
    }

    #[test]
    fn nan_becomes_silence_not_a_random_sample() {
        assert_eq!(f32_to_i16(f32::NAN), 0);
    }

    #[test]
    fn mid_scale_is_roughly_half() {
        let half = f32_to_i16(0.5);
        assert!((half as i32 - 16_383).abs() <= 2, "got {half}");
    }

    #[test]
    fn conversion_is_symmetric_about_zero() {
        // An asymmetric conversion adds a DC offset, which shows up as a click on
        // every buffer boundary.
        for v in [0.1f32, 0.25, 0.5, 0.75, 1.0] {
            assert_eq!(f32_to_i16(v), -f32_to_i16(-v), "asymmetric at {v}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn non_windows_reports_unsupported() {
        assert!(matches!(
            spawn_loopback_capture().err(),
            Some(LoopbackError::Unsupported)
        ));
    }
}
