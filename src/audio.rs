//! Microphone capture, speaker playback and sample-rate conversion on top of cpal.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<без назви>".into())
}

pub fn list_devices() -> Result<()> {
    let host = cpal::default_host();
    let default_in = host.default_input_device().map(|d| device_name(&d));
    let default_out = host.default_output_device().map(|d| device_name(&d));

    println!("Мікрофони (--input-device):");
    for d in host.input_devices()? {
        let name = device_name(&d);
        let mark = if Some(&name) == default_in.as_ref() { "*" } else { " " };
        println!("  {mark} {name}");
    }
    println!("\nВиходи звуку (--output-device):");
    for d in host.output_devices()? {
        let name = device_name(&d);
        let mark = if Some(&name) == default_out.as_ref() { "*" } else { " " };
        println!("  {mark} {name}");
    }
    println!("\n* — пристрій за замовчуванням. Для вибору досить частини назви.");
    Ok(())
}

fn pick_device(wanted: Option<&str>, input: bool) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let kind = if input { "мікрофон" } else { "вихід звуку" };
    match wanted {
        None => if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| anyhow!("у системі немає пристрою: {kind}")),
        Some(w) => {
            let w = w.to_lowercase();
            let devices: Vec<cpal::Device> = if input {
                host.input_devices()?.collect()
            } else {
                host.output_devices()?.collect()
            };
            devices
                .into_iter()
                .find(|d| device_name(d).to_lowercase().contains(&w))
                .ok_or_else(|| anyhow!("{kind} з «{w}» у назві не знайдено; список: live-interpreter devices"))
        }
    }
}

/// Streaming linear resampler with a box low-pass filter when downsampling.
pub struct Resampler {
    step: f64,
    t: f64,
    buf: Vec<f32>,
    box_len: usize,
    box_hist: VecDeque<f32>,
    box_sum: f64,
}

impl Resampler {
    pub fn new(src_rate: u32, dst_rate: u32) -> Self {
        let step = src_rate as f64 / dst_rate as f64;
        let box_len = if step > 1.5 { step.round() as usize } else { 1 };
        Self { step, t: 0.0, buf: Vec::new(), box_len, box_hist: VecDeque::new(), box_sum: 0.0 }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &x in input {
            let y = if self.box_len > 1 {
                self.box_hist.push_back(x);
                self.box_sum += x as f64;
                if self.box_hist.len() > self.box_len {
                    self.box_sum -= self.box_hist.pop_front().unwrap_or(0.0) as f64;
                }
                (self.box_sum / self.box_hist.len() as f64) as f32
            } else {
                x
            };
            self.buf.push(y);
        }
        while (self.t as usize) + 1 < self.buf.len() {
            let i = self.t as usize;
            let frac = (self.t - i as f64) as f32;
            out.push(self.buf[i] * (1.0 - frac) + self.buf[i + 1] * frac);
            self.t += self.step;
        }
        let consumed = (self.t as usize).min(self.buf.len().saturating_sub(1));
        self.buf.drain(..consumed);
        self.t -= consumed as f64;
    }
}

/// A running microphone. Mono f32 blocks at `rate` arrive on the receiver returned by `start`.
pub struct Mic {
    _stream: Stream,
    pub rate: u32,
    pub name: String,
}

impl Mic {
    pub fn start(wanted: Option<&str>) -> Result<(Mic, mpsc::UnboundedReceiver<Vec<f32>>)> {
        let device = pick_device(wanted, true)?;
        let name = device_name(&device);
        let supported = device
            .default_input_config()
            .with_context(|| format!("не вдалося прочитати формат мікрофона «{name}»"))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let channels = config.channels as usize;
        let rate = config.sample_rate;
        let (tx, rx) = mpsc::unbounded_channel();
        let stream = match format {
            SampleFormat::F32 => build_input::<f32>(&device, config, channels, tx)?,
            SampleFormat::I16 => build_input::<i16>(&device, config, channels, tx)?,
            SampleFormat::U16 => build_input::<u16>(&device, config, channels, tx)?,
            SampleFormat::I32 => build_input::<i32>(&device, config, channels, tx)?,
            other => return Err(anyhow!("мікрофон віддає непідтримуваний формат {other:?}")),
        };
        stream.play().context("не вдалося запустити мікрофон")?;
        Ok((Mic { _stream: stream, rate, name }, rx))
    }
}

fn build_input<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    tx: mpsc::UnboundedSender<Vec<f32>>,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let stream = device.build_input_stream::<T, _, _>(
        config,
        move |data: &[T], _| {
            let mono: Vec<f32> = data
                .chunks(channels)
                .map(|frame| frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / channels as f32)
                .collect();
            let _ = tx.send(mono);
        },
        // A short xrun while the device starts is harmless; report only real failures.
        |e: cpal::Error| {
            if e.kind() != cpal::ErrorKind::Xrun {
                eprintln!("\nпомилка мікрофона: {e}");
            }
        },
        None,
    )?;
    Ok(stream)
}

/// Speaker output. Keep the `Player` alive on the main task; hand `PlayerHandle`s to other tasks.
pub struct Player {
    _stream: Option<Stream>,
    handle: PlayerHandle,
    pub name: String,
}

#[derive(Clone)]
pub struct PlayerHandle {
    queue: Arc<Mutex<VecDeque<f32>>>,
    last_play_ms: Arc<AtomicU64>,
    start: Instant,
    pub rate: u32,
    muted: bool,
}

impl Player {
    pub fn start(wanted: Option<&str>, muted: bool) -> Result<Player> {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let last_play_ms = Arc::new(AtomicU64::new(0));
        let start = Instant::now();
        if muted {
            let handle = PlayerHandle { queue, last_play_ms, start, rate: 24_000, muted };
            return Ok(Player { _stream: None, handle, name: "(звук вимкнено)".into() });
        }
        let device = pick_device(wanted, false)?;
        let name = device_name(&device);
        let supported = device
            .default_output_config()
            .with_context(|| format!("не вдалося прочитати формат виходу «{name}»"))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.config();
        let channels = config.channels as usize;
        let rate = config.sample_rate;
        let (q, lp) = (queue.clone(), last_play_ms.clone());
        let stream = match format {
            SampleFormat::F32 => build_output::<f32>(&device, config, channels, q, lp, start)?,
            SampleFormat::I16 => build_output::<i16>(&device, config, channels, q, lp, start)?,
            SampleFormat::U16 => build_output::<u16>(&device, config, channels, q, lp, start)?,
            SampleFormat::I32 => build_output::<i32>(&device, config, channels, q, lp, start)?,
            other => return Err(anyhow!("вихід звуку хоче непідтримуваний формат {other:?}")),
        };
        stream.play().context("не вдалося запустити відтворення")?;
        let handle = PlayerHandle { queue, last_play_ms, start, rate, muted };
        Ok(Player { _stream: Some(stream), handle, name })
    }

    pub fn handle(&self) -> PlayerHandle {
        self.handle.clone()
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    queue: Arc<Mutex<VecDeque<f32>>>,
    last_play_ms: Arc<AtomicU64>,
    start: Instant,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let stream = device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            let mut q = queue.lock().unwrap();
            let mut played = false;
            for frame in data.chunks_mut(channels) {
                let s = match q.pop_front() {
                    Some(v) => {
                        played = true;
                        v
                    }
                    None => 0.0,
                };
                let v = T::from_sample(s);
                for out in frame.iter_mut() {
                    *out = v;
                }
            }
            if played {
                last_play_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        },
        |e: cpal::Error| {
            if e.kind() != cpal::ErrorKind::Xrun {
                eprintln!("\nпомилка відтворення: {e}");
            }
        },
        None,
    )?;
    Ok(stream)
}

impl PlayerHandle {
    /// Queue mono samples that are already at `self.rate`.
    pub fn push(&self, samples: &[f32]) {
        if self.muted {
            return;
        }
        self.queue.lock().unwrap().extend(samples.iter().copied());
    }

    /// True while audio is queued or was audible within the last `hold`.
    pub fn busy(&self, hold: Duration) -> bool {
        if self.muted {
            return false;
        }
        if !self.queue.lock().unwrap().is_empty() {
            return true;
        }
        let last = self.last_play_ms.load(Ordering::Relaxed);
        last != 0 && (self.start.elapsed().as_millis() as u64) < last + hold.as_millis() as u64
    }

    pub async fn wait_idle(&self) {
        while self.busy(Duration::from_millis(200)) {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Read a WAV (any rate/channels) or raw 16 kHz mono s16le `.pcm` file as mono f32.
pub fn read_audio_file(path: &std::path::Path) -> Result<(Vec<f32>, u32)> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext == "pcm" {
        let bytes = std::fs::read(path)?;
        let samples = bytes
            .chunks_exact(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
            .collect();
        return Ok((samples, 16_000));
    }
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("не вдалося відкрити {} (потрібен WAV або 16 кГц .pcm)", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.map(|v| v as f32 / scale)).collect::<Result<_, _>>()?
        }
    };
    let mono = interleaved
        .chunks(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

pub fn f32_to_i16(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0) as i16
}
