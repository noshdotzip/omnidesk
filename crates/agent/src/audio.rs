//! Audio routing between machines.
//!
//! Sends one machine's audio output to another, in either direction: Linux captures
//! a PipeWire sink's monitor, Windows captures its render endpoint via WASAPI loopback,
//! and the receiving end plays it locally. Either machine can be either end.
//!
//! # Why this rides its own connection
//! The peer control channel is line-delimited JSON, which is the wrong shape for a
//! continuous binary stream, and audio has completely different latency and ordering
//! needs from input events. Keeping the media plane separate from the control plane also
//! matches the split the architecture already assumes (ADR-0003).
//!
//! # Format
//! A single JSON header line, then raw little-endian interleaved `s16` frames until the
//! connection closes. Verified against `pw-record` on Arch: at 48 kHz stereo that is
//! exactly 192,000 bytes per second.
//!
//! # Status
//! Capture uses the `pw-record` CLI rather than a native PipeWire client. That is a
//! deliberate first cut: it proves the path and the format end to end without pulling in
//! a PipeWire binding, and it is the piece to replace when latency is measured and found
//! wanting; the same applies to `pw-play` on the receiving side. There is no encoding,
//! so this needs ~1.5 Mbit/s — fine on the measured LAN
//! (167+ Mbit/s), not fine over anything slower.

// One protocol, several platform halves: a sender encodes a header and streams PCM,
// a receiver parses and decodes it, and which of those exist depends on the target
// (PipeWire capture and playback on Linux, WASAPI playback on Windows). Every build
// compiles all the helpers but calls only the subset its platform provides, so the
// dead-code warnings here are structural rather than real. The unit tests exercise
// all of it everywhere, which is what actually keeps the ends agreeing on the wire
// format.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// PCM stream description. Only signed 16-bit little-endian interleaved is supported;
/// the field exists so a receiver rejects anything else loudly instead of playing noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
}

impl Default for AudioFormat {
    fn default() -> Self {
        AudioFormat {
            rate: 48_000,
            channels: 2,
        }
    }
}

impl AudioFormat {
    pub const BYTES_PER_SAMPLE: usize = 2;

    pub fn bytes_per_frame(&self) -> usize {
        self.channels as usize * Self::BYTES_PER_SAMPLE
    }

    pub fn bytes_per_second(&self) -> usize {
        self.rate as usize * self.bytes_per_frame()
    }

    /// Whole frames contained in `bytes`. A partial trailing frame is not a frame:
    /// treating one as complete shifts channel alignment and swaps left with right for
    /// the rest of the stream.
    pub fn frames_in(&self, bytes: usize) -> usize {
        bytes / self.bytes_per_frame()
    }

    /// Bytes needed to hold `ms` of audio, rounded down to a whole frame.
    pub fn bytes_for_ms(&self, ms: f64) -> usize {
        if ms <= 0.0 {
            return 0;
        }
        let raw = (self.bytes_per_second() as f64 * ms / 1000.0) as usize;
        raw - (raw % self.bytes_per_frame())
    }

    /// How much wall time `bytes` of audio represents.
    pub fn duration_ms(&self, bytes: usize) -> f64 {
        if self.bytes_per_second() == 0 {
            return 0.0;
        }
        bytes as f64 * 1000.0 / self.bytes_per_second() as f64
    }

    pub fn is_supported(&self) -> bool {
        self.rate >= 8_000 && self.rate <= 192_000 && (self.channels == 1 || self.channels == 2)
    }
}

/// The one header line that precedes the PCM bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioHeader {
    pub format: AudioFormat,
    /// Always "s16le". Present so a future change is a rejection, not silent garbage.
    #[serde(default = "default_encoding")]
    pub encoding: EncodingTag,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EncodingTag {
    S16Le,
}

fn default_encoding() -> EncodingTag {
    EncodingTag::S16Le
}

impl AudioHeader {
    pub fn new(format: AudioFormat) -> Self {
        AudioHeader {
            format,
            encoding: EncodingTag::S16Le,
        }
    }

    pub fn encode(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }

    pub fn parse(line: &str) -> Result<Self, String> {
        let header: AudioHeader =
            serde_json::from_str(line.trim()).map_err(|e| format!("bad audio header: {e}"))?;
        if !header.format.is_supported() {
            return Err(format!(
                "unsupported audio format: {} Hz, {} channels",
                header.format.rate, header.format.channels
            ));
        }
        Ok(header)
    }
}

/// Convert a little-endian `s16` byte buffer into samples.
///
/// Any trailing partial frame is left for the caller to carry into the next read: TCP
/// splits wherever it likes, and decoding a half-frame permanently swaps the channels.
pub fn decode_s16le(bytes: &[u8], format: AudioFormat, out: &mut Vec<i16>) -> usize {
    let frame = format.bytes_per_frame();
    let usable = bytes.len() - (bytes.len() % frame);
    for chunk in bytes[..usable].chunks_exact(2) {
        out.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
    usable
}

// ---- sender: Linux / PipeWire ------------------------------------------------------

/// Capture a PipeWire sink's monitor and stream it to a peer.
///
/// `target` is normally `<sink-name>.monitor` — the monitor of an *output*, so what the
/// machine is playing rather than a microphone. Capturing the sink itself, or an input
/// node, silently sends the wrong audio.
#[cfg(target_os = "linux")]
pub async fn send(addr: &str, target: &str, format: AudioFormat) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    anyhow::ensure!(format.is_supported(), "unsupported capture format");

    let mut child = tokio::process::Command::new("pw-record")
        .arg(format!("--target={target}"))
        .arg(format!("--rate={}", format.rate))
        .arg(format!("--channels={}", format.channels))
        .arg("--format=s16")
        .arg("--raw")
        .arg("-")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start pw-record (is pipewire installed?)")?;

    let mut audio = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("pw-record produced no stdout"))?;

    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("could not connect to audio peer at {addr}"))?;
    stream
        .write_all(AudioHeader::new(format).encode()?.as_bytes())
        .await?;
    stream.flush().await?;

    tracing::info!(
        target = %target,
        rate = format.rate,
        channels = format.channels,
        "streaming audio to peer"
    );
    println!(
        "streaming {} Hz x{} to {addr}",
        format.rate, format.channels
    );

    let copied = tokio::io::copy(&mut audio, &mut stream).await;
    // Killing pw-record matters: a leaked capture keeps a PipeWire stream open and
    // shows up in the user's volume mixer forever.
    let _ = child.kill().await;
    let bytes = copied.context("audio stream ended with an error")?;
    println!(
        "sent {:.1} MB, {} frames (~{:.1}s of audio)",
        bytes as f64 / 1e6,
        format.frames_in(bytes as usize),
        format.duration_ms(bytes as usize) / 1000.0
    );
    Ok(())
}

/// Capture what this machine is playing (WASAPI loopback) and stream it to a peer.
///
/// `requested` is **ignored**: in shared mode the endpoint's mix format is not
/// negotiable, so the capture reports what the device actually gave us and that is what
/// goes in the header. Claiming the requested format instead would mislabel the stream
/// and the receiver would play it at the wrong speed.
#[cfg(windows)]
pub async fn send(addr: &str, _target: &str, requested: AudioFormat) -> anyhow::Result<()> {
    use anyhow::Context;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use ultidesk_platform_windows::loopback::spawn_loopback_capture;

    let capture = spawn_loopback_capture().context("starting WASAPI loopback capture")?;
    let format = AudioFormat {
        rate: capture.rate,
        channels: capture.channels,
    };
    if format.rate != requested.rate || format.channels != requested.channels {
        println!(
            "note: device mixes at {} Hz x{}; streaming that rather than the requested {} Hz x{}",
            format.rate, format.channels, requested.rate, requested.channels
        );
    }
    // A 5.1 endpoint would need a downmix, which is not implemented. Failing here beats
    // sending six interleaved channels labelled as two and playing noise.
    anyhow::ensure!(
        format.is_supported(),
        "the default output mixes at {} Hz x{} channels, which this stream format does not \
         cover (mono/stereo only). Set the Windows output to stereo, or implement a downmix.",
        format.rate,
        format.channels
    );

    let mut stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("could not connect to audio peer at {addr}"))?;
    stream
        .write_all(AudioHeader::new(format).encode()?.as_bytes())
        .await?;
    stream.flush().await?;

    println!(
        "streaming {} Hz x{} to {addr}",
        format.rate, format.channels
    );
    let mut total = 0usize;
    let mut bytes = Vec::new();

    // A peer hanging up is the normal way this ends, not a failure, so the write
    // error breaks the loop and the summary still prints. Only losing the capture
    // thread is an actual error.
    let outcome: anyhow::Result<()> = loop {
        match capture.samples.try_recv() {
            Ok(samples) => {
                bytes.clear();
                bytes.reserve(samples.len() * AudioFormat::BYTES_PER_SAMPLE);
                for s in samples {
                    bytes.extend_from_slice(&s.to_le_bytes());
                }
                if stream.write_all(&bytes).await.is_err() {
                    break Ok(()); // peer closed the connection
                }
                total += bytes.len();
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                // Loopback produces nothing while the endpoint is idle.
                tokio::time::sleep(std::time::Duration::from_millis(4)).await;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                break Err(anyhow::anyhow!("the loopback capture thread stopped"));
            }
        }
    };

    println!(
        "sent {:.1} MB, {} frames (~{:.1}s)",
        total as f64 / 1e6,
        format.frames_in(total),
        format.duration_ms(total) / 1000.0
    );
    outcome
}
#[cfg(not(any(windows, target_os = "linux")))]
pub async fn send(_addr: &str, _target: &str, _format: AudioFormat) -> anyhow::Result<()> {
    anyhow::bail!("audio capture needs PipeWire (Linux) or WASAPI loopback (Windows)")
}

// ---- receiver: Windows / WASAPI via cpal -------------------------------------------

/// Accept one audio stream and play it on the default output device.
#[cfg(windows)]
pub async fn recv(bind: &str, latency_ms: f64) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;
    println!("waiting for an audio peer on {bind}");

    let (stream, peer) = listener.accept().await?;
    println!("audio peer connected: {peer}");
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let header = AudioHeader::parse(&line).map_err(|e| anyhow::anyhow!(e))?;
    let format = header.format;
    println!("stream: {} Hz x{}", format.rate, format.channels);

    // Bounded so a sender that outruns the sound card cannot grow latency without
    // limit. Dropping the oldest audio keeps the stream live at the cost of a glitch,
    // which is the right trade for a live feed.
    let max_samples = format.bytes_for_ms(latency_ms.max(20.0)) / AudioFormat::BYTES_PER_SAMPLE;
    let buffer: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::new()));

    // cpal's Stream is not Send, so it lives on its own thread and stays there.
    let playback = buffer.clone();
    let (err_tx, err_rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("ultidesk-audio-out".into())
        .spawn(move || {
            use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
            let host = cpal::default_host();
            let Some(device) = host.default_output_device() else {
                let _ = err_tx.send("no default audio output device".into());
                return;
            };
            let config = cpal::StreamConfig {
                channels: format.channels,
                sample_rate: cpal::SampleRate(format.rate),
                buffer_size: cpal::BufferSize::Default,
            };
            let stream = device.build_output_stream(
                &config,
                move |out: &mut [i16], _: &cpal::OutputCallbackInfo| {
                    let mut buf = playback
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    for slot in out.iter_mut() {
                        // Silence on underrun. Repeating the last sample would be a
                        // buzz; silence is a gap, which is far less unpleasant.
                        *slot = buf.pop_front().unwrap_or(0);
                    }
                },
                move |e| tracing::warn!(error = %e, "audio output error"),
                None,
            );
            let stream = match stream {
                Ok(s) => s,
                Err(e) => {
                    let _ = err_tx.send(format!("could not open the output device: {e}"));
                    return;
                }
            };
            if let Err(e) = stream.play() {
                let _ = err_tx.send(format!("could not start playback: {e}"));
                return;
            }
            let _ = err_tx.send(String::new()); // ready
                                                // The Stream stops when dropped, so park to keep it alive.
            loop {
                std::thread::park();
            }
        })
        .context("spawning the audio output thread")?;

    match err_rx.recv() {
        Ok(msg) if msg.is_empty() => {}
        Ok(msg) => anyhow::bail!(msg),
        Err(e) => anyhow::bail!("audio output thread died: {e}"),
    }
    println!("playing (latency cap {latency_ms:.0} ms) — Ctrl+C to stop");

    let mut raw = vec![0u8; 8192];
    let mut carry: Vec<u8> = Vec::new();
    let mut samples: Vec<i16> = Vec::new();
    let mut total = 0usize;

    loop {
        let n = reader.read(&mut raw).await?;
        if n == 0 {
            break;
        }
        total += n;
        carry.extend_from_slice(&raw[..n]);
        samples.clear();
        let used = decode_s16le(&carry, format, &mut samples);
        carry.drain(..used);

        let mut buf = buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        buf.extend(samples.iter().copied());
        while buf.len() > max_samples {
            buf.pop_front();
        }
    }

    println!(
        "audio peer disconnected after {:.1} MB, {} frames (~{:.1}s)",
        total as f64 / 1e6,
        format.frames_in(total),
        format.duration_ms(total) / 1000.0
    );
    Ok(())
}

/// Accept one audio stream and play it through PipeWire.
///
/// Symmetric with [`send`]: `pw-play` is the counterpart to `pw-record`, and PipeWire
/// does its own buffering, so `latency_ms` is not used here. Replacing both with a
/// native PipeWire client is the same follow-up.
#[cfg(target_os = "linux")]
pub async fn recv(bind: &str, _latency_ms: f64) -> anyhow::Result<()> {
    use anyhow::Context;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind {bind}"))?;
    println!("waiting for an audio peer on {bind}");

    let (stream, peer) = listener.accept().await?;
    println!("audio peer connected: {peer}");
    let mut reader = BufReader::new(stream);

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let header = AudioHeader::parse(&line).map_err(|e| anyhow::anyhow!(e))?;
    let format = header.format;
    println!("stream: {} Hz x{}", format.rate, format.channels);

    let mut child = tokio::process::Command::new("pw-play")
        .arg(format!("--rate={}", format.rate))
        .arg(format!("--channels={}", format.channels))
        .arg("--format=s16")
        .arg("--raw")
        .arg("-")
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("could not start pw-play (is pipewire installed?)")?;

    let mut sink = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("pw-play accepted no stdin"))?;

    println!("playing via pw-play — Ctrl+C to stop");

    // Counted manually rather than with tokio::io::copy so the total survives an
    // error: a stream that breaks midway is exactly when knowing how much arrived
    // matters, and copy() only reports its count on success. A reset is also how a
    // hard-killed sender looks, which is ordinary rather than exceptional.
    let mut total = 0usize;
    let mut buf = vec![0u8; 8192];
    let outcome = loop {
        match reader.read(&mut buf).await {
            Ok(0) => break Ok(()),
            Ok(n) => {
                total += n;
                if sink.write_all(&buf[..n]).await.is_err() {
                    break Err(anyhow::anyhow!("pw-play stopped accepting audio"));
                }
            }
            Err(e) => break Err(anyhow::Error::from(e)),
        }
    };

    // Dropping stdin lets pw-play drain and exit rather than linger holding a stream.
    drop(sink);
    let _ = child.wait().await;

    println!(
        "audio peer disconnected after {:.1} MB, {} frames (~{:.1}s)",
        total as f64 / 1e6,
        format.frames_in(total),
        format.duration_ms(total) / 1000.0
    );
    // A peer that vanished is not a failure worth a non-zero exit; a broken pipe to
    // pw-play is.
    match outcome {
        Ok(()) => Ok(()),
        Err(e) if e.downcast_ref::<std::io::Error>().is_some() => {
            println!("(stream ended abruptly: {e})");
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub async fn recv(_bind: &str, _latency_ms: f64) -> anyhow::Result<()> {
    anyhow::bail!("audio playback needs WASAPI (Windows) or PipeWire (Linux)")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CD: AudioFormat = AudioFormat {
        rate: 48_000,
        channels: 2,
    };

    #[test]
    fn frame_and_rate_math_matches_the_measured_stream() {
        // pw-record at 48kHz stereo s16 produced exactly 192000 bytes/sec on the Arch
        // box; if this drifts, the receiver's latency estimate silently drifts with it.
        assert_eq!(CD.bytes_per_frame(), 4);
        assert_eq!(CD.bytes_per_second(), 192_000);
        assert_eq!(CD.bytes_for_ms(1000.0), 192_000);
        assert_eq!(CD.bytes_for_ms(10.0), 1_920);
        assert!((CD.duration_ms(192_000) - 1000.0).abs() < 0.001);
    }

    #[test]
    fn bytes_for_ms_always_lands_on_a_frame_boundary() {
        // A buffer bound that is not frame-aligned eventually splits a frame and swaps
        // the channels for the rest of the stream.
        for ms in [0.5, 1.0, 3.3, 7.7, 100.0] {
            assert_eq!(CD.bytes_for_ms(ms) % CD.bytes_per_frame(), 0, "{ms}ms");
        }
        let mono = AudioFormat {
            rate: 44_100,
            channels: 1,
        };
        assert_eq!(mono.bytes_for_ms(3.3) % mono.bytes_per_frame(), 0);
    }

    #[test]
    fn partial_frames_are_not_counted() {
        assert_eq!(CD.frames_in(4), 1);
        assert_eq!(CD.frames_in(7), 1); // 3 trailing bytes are not a frame
        assert_eq!(CD.frames_in(3), 0);
    }

    #[test]
    fn non_positive_durations_yield_no_bytes() {
        assert_eq!(CD.bytes_for_ms(0.0), 0);
        assert_eq!(CD.bytes_for_ms(-5.0), 0);
    }

    #[test]
    fn header_round_trips() {
        let h = AudioHeader::new(CD);
        let line = h.encode().expect("encode");
        assert!(line.ends_with('\n'));
        assert_eq!(AudioHeader::parse(&line).expect("parse"), h);
    }

    #[test]
    fn implausible_formats_are_rejected_rather_than_played_as_noise() {
        let bad = AudioHeader::new(AudioFormat {
            rate: 3,
            channels: 2,
        });
        let line = serde_json::to_string(&bad).unwrap();
        assert!(AudioHeader::parse(&line).is_err());

        let too_many = AudioHeader::new(AudioFormat {
            rate: 48_000,
            channels: 9,
        });
        let line = serde_json::to_string(&too_many).unwrap();
        assert!(AudioHeader::parse(&line).is_err());
    }

    #[test]
    fn garbage_headers_do_not_panic() {
        assert!(AudioHeader::parse("not json").is_err());
        assert!(AudioHeader::parse("").is_err());
        assert!(AudioHeader::parse("{}").is_err());
    }

    #[test]
    fn decode_consumes_only_whole_frames_and_reports_how_many_bytes_it_used() {
        // TCP splits wherever it likes; the caller must be able to carry the remainder.
        let mut out = Vec::new();
        // 6 bytes = one stereo frame (4) plus half of the next.
        let used = decode_s16le(&[1, 0, 2, 0, 3, 0], CD, &mut out);
        assert_eq!(used, 4, "must not consume a partial frame");
        assert_eq!(out, vec![1i16, 2i16]);
    }

    #[test]
    fn decode_is_little_endian() {
        let mut out = Vec::new();
        // 0x0100 little-endian is 256, not 1.
        decode_s16le(&[0x00, 0x01, 0x00, 0x01], CD, &mut out);
        assert_eq!(out, vec![256i16, 256i16]);
    }

    #[test]
    fn decode_handles_an_empty_read() {
        let mut out = Vec::new();
        assert_eq!(decode_s16le(&[], CD, &mut out), 0);
        assert!(out.is_empty());
    }
}
