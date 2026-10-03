//! Audio output that survives the device going away (suspend/resume, unplugged USB DAC,
//! PipeWire restart).
//!
//! librespot's stock rodio sink opens the device once for the Player's lifetime and installs
//! rodio's default stream error handler, which only prints. A stream killed by suspend
//! (`alsa::poll() returned POLLERR`) then stays dead: samples are never consumed, the player
//! thread spins in its backpressure loop, and stderr fills with the same error. This sink opens
//! the device when playback starts, releases it on stop, and reopens it whenever the stream
//! reports an error or stops draining.

use librespot_playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use rodio::{OutputStream, OutputStreamBuilder, buffer::SamplesBuffer};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Queued packets allowed ahead of the device (~0.5s; packets are ~256-3000 samples).
const MAX_QUEUED: usize = 26;
/// A stream that consumes nothing for this long is treated as dead even without an error.
const STALL: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);

struct Output {
    // Field order matters: the sink must drop before the stream it plays into.
    sink: rodio::Sink,
    _stream: OutputStream,
    dead: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct ResilientSink {
    out: Option<Output>,
}

impl ResilientSink {
    fn open_output() -> Result<Output, String> {
        let dead = Arc::new(AtomicBool::new(false));
        let flag = dead.clone();
        let mut stream = OutputStreamBuilder::from_default_device()
            .map_err(|e| e.to_string())?
            .with_channels(NUM_CHANNELS as _)
            .with_sample_rate(SAMPLE_RATE)
            .with_error_callback(move |e| {
                if !flag.swap(true, Ordering::Relaxed) {
                    tracing::warn!("audio stream error, will reopen the device: {e}");
                }
            })
            .open_stream_or_fallback()
            .map_err(|e| e.to_string())?;
        stream.log_on_drop(false);
        let sink = rodio::Sink::connect_new(stream.mixer());
        Ok(Output { sink, _stream: stream, dead })
    }

    /// The live output, (re)opening it if absent or flagged dead.
    fn output(&mut self) -> SinkResult<&mut Output> {
        if self.out.as_ref().is_some_and(|o| o.dead.load(Ordering::Relaxed)) {
            self.out = None;
        }
        if self.out.is_none() {
            let out = Self::open_output().map_err(SinkError::NotConnected)?;
            tracing::info!("audio device opened");
            self.out = Some(out);
        }
        Ok(self.out.as_mut().expect("opened above"))
    }
}

impl Sink for ResilientSink {
    fn start(&mut self) -> SinkResult<()> {
        self.output()?.sink.play();
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        if let Some(out) = self.out.take() {
            // Let the queued audio play out, but never wait on a dead or stalled stream.
            let started = Instant::now();
            while out.sink.len() > 0 && !out.dead.load(Ordering::Relaxed) && started.elapsed() < STALL {
                std::thread::sleep(POLL);
            }
            // Dropping releases the device, so nothing is held (or left dead) while paused.
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        let samples = packet.samples().map_err(|e| SinkError::OnWrite(e.to_string()))?;
        let samples: &[f32] = &converter.f64_to_f32(samples);
        self.output()?.sink.append(SamplesBuffer::new(NUM_CHANNELS as _, SAMPLE_RATE, samples));

        let mut last_len = usize::MAX;
        let mut progress = Instant::now();
        loop {
            let out = self.out.as_ref().expect("opened by output()");
            let len = out.sink.len();
            if len <= MAX_QUEUED {
                return Ok(());
            }
            if out.dead.load(Ordering::Relaxed) || progress.elapsed() >= STALL {
                // Drop the dead stream; the queued tail is lost, the next write reopens.
                self.out = None;
                return Ok(());
            }
            if len != last_len {
                last_len = len;
                progress = Instant::now();
            }
            std::thread::sleep(POLL);
        }
    }
}
