//! Microphone capture, playback to one or more outputs, and sample-rate conversion on top of cpal.

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

/// Names of the input devices and of the default one.
pub fn input_names() -> (Vec<String>, Option<String>) {
    let host = cpal::default_host();
    let names = host.input_devices().map(|d| d.map(|d| device_name(&d)).collect()).unwrap_or_default();
    (names, host.default_input_device().map(|d| device_name(&d)))
}

/// Names of the output devices and of the default one.
pub fn output_names() -> (Vec<String>, Option<String>) {
    let host = cpal::default_host();
    let names = host.output_devices().map(|d| d.map(|d| device_name(&d)).collect()).unwrap_or_default();
    (names, host.default_output_device().map(|d| device_name(&d)))
}

pub fn list_devices() -> Result<()> {
    let print = |title: &str, (names, default): (Vec<String>, Option<String>)| {
        println!("{title}");
        for name in names {
            let mark = if Some(&name) == default.as_ref() { "*" } else { " " };
            println!("  {mark} {name}");
        }
    };
    print("Мікрофони (--input-device):", input_names());
    println!();
    print("Виходи звуку (--output-device, --mic-output):", output_names());
    println!("\n* — пристрій за замовчуванням. Для вибору досить частини назви.");
    Ok(())
}

/// Finds a device by exact name first, then by a case-insensitive part of the name.
fn pick_device(wanted: Option<&str>, input: bool) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let kind = if input { "мікрофон" } else { "вихід звуку" };
    let Some(wanted) = wanted else {
        return if input { host.default_input_device() } else { host.default_output_device() }
            .ok_or_else(|| anyhow!("у системі немає пристрою: {kind}"));
    };
    let devices: Vec<cpal::Device> =
        if input { host.input_devices()?.collect() } else { host.output_devices()?.collect() };
    let lower = wanted.to_lowercase();
    let exact = devices.iter().position(|d| device_name(d) == wanted);
    let partial = || devices.iter().position(|d| device_name(d).to_lowercase().contains(&lower));
    match exact.or_else(partial) {
        Some(i) => Ok(devices.into_iter().nth(i).unwrap()),
        None => Err(anyhow!("{kind} «{wanted}» не знайдено")),
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

/// One playback device and the samples waiting for it, at the device's own rate.
struct Output {
    queue: Mutex<VecDeque<f32>>,
    rate: u32,
    last_play_ms: AtomicU64,
}

/// Plays the same speech on every output: for example headphones to listen and Voicemeeter's
/// virtual input to act as a microphone. Keep the `Player` alive on the task that created it
/// (cpal streams may not move between threads) and hand `PlayerHandle`s to other tasks.
pub struct Player {
    _streams: Vec<Stream>,
    handle: PlayerHandle,
    pub names: Vec<String>,
}

#[derive(Clone)]
pub struct PlayerHandle {
    outputs: Arc<Vec<Output>>,
    start: Instant,
}

/// Which device to open: `None` means the system default output.
pub type OutputChoice = Option<String>;

impl Player {
    pub fn start(choices: &[OutputChoice]) -> Result<Player> {
        let start = Instant::now();
        let mut outputs = Vec::new();
        let mut devices = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for choice in choices {
            let device = pick_device(choice.as_deref(), false)?;
            let name = device_name(&device);
            if names.contains(&name) {
                continue; // the default output and an explicit choice can be the same device
            }
            let supported = device
                .default_output_config()
                .with_context(|| format!("не вдалося прочитати формат виходу «{name}»"))?;
            outputs.push(Output {
                queue: Mutex::new(VecDeque::new()),
                rate: supported.config().sample_rate,
                last_play_ms: AtomicU64::new(0),
            });
            devices.push((device, supported));
            names.push(name);
        }
        let outputs = Arc::new(outputs);
        let mut streams = Vec::new();
        for (index, (device, supported)) in devices.into_iter().enumerate() {
            let format = supported.sample_format();
            let config: StreamConfig = supported.config();
            let channels = config.channels as usize;
            let (o, name) = (outputs.clone(), &names[index]);
            let stream = match format {
                SampleFormat::F32 => build_output::<f32>(&device, config, channels, o, index, start)?,
                SampleFormat::I16 => build_output::<i16>(&device, config, channels, o, index, start)?,
                SampleFormat::U16 => build_output::<u16>(&device, config, channels, o, index, start)?,
                SampleFormat::I32 => build_output::<i32>(&device, config, channels, o, index, start)?,
                other => return Err(anyhow!("вихід «{name}» хоче непідтримуваний формат {other:?}")),
            };
            stream.play().with_context(|| format!("не вдалося запустити відтворення на «{name}»"))?;
            streams.push(stream);
        }
        Ok(Player { _streams: streams, handle: PlayerHandle { outputs, start }, names })
    }

    pub fn handle(&self) -> PlayerHandle {
        self.handle.clone()
    }
}

fn build_output<T>(
    device: &cpal::Device,
    config: StreamConfig,
    channels: usize,
    outputs: Arc<Vec<Output>>,
    index: usize,
    start: Instant,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let stream = device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            let output = &outputs[index];
            let mut q = output.queue.lock().unwrap();
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
                output.last_play_ms.store(start.elapsed().as_millis() as u64, Ordering::Relaxed);
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
    /// One resampler per output, for a stream of samples at `src_rate`.
    pub fn resamplers(&self, src_rate: u32) -> Vec<Resampler> {
        self.outputs.iter().map(|o| Resampler::new(src_rate, o.rate)).collect()
    }

    /// Queues the same mono samples on every output, converting them with `resamplers`.
    pub fn push(&self, samples: &[f32], resamplers: &mut [Resampler]) {
        let mut converted = Vec::with_capacity(samples.len() * 2);
        for (output, resampler) in self.outputs.iter().zip(resamplers.iter_mut()) {
            converted.clear();
            resampler.process(samples, &mut converted);
            output.queue.lock().unwrap().extend(converted.iter().copied());
        }
    }

    /// True while audio is queued on any output or was audible within the last `hold`.
    pub fn busy(&self, hold: Duration) -> bool {
        let now = self.start.elapsed().as_millis() as u64;
        self.outputs.iter().any(|o| {
            let last = o.last_play_ms.load(Ordering::Relaxed);
            !o.queue.lock().unwrap().is_empty() || (last != 0 && now < last + hold.as_millis() as u64)
        })
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
