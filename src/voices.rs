//! Voice lookup, Instant Voice Cloning and recording voice samples.

use crate::audio::{Mic, f32_to_i16};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const API: &str = "https://api.elevenlabs.io";

async fn get_json(http: &reqwest::Client, key: &str, url: &str) -> Result<Value> {
    let resp = http.get(url).header("xi-api-key", key).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        bail!("ElevenLabs {status}: {text}");
    }
    Ok(serde_json::from_str(&text)?)
}

pub struct Voice {
    pub id: String,
    pub name: String,
    pub category: String,
}

fn parse_voices(v: &Value) -> Vec<Voice> {
    v["voices"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|x| Voice {
                    id: x["voice_id"].as_str().unwrap_or("").to_string(),
                    name: x["name"].as_str().unwrap_or("").to_string(),
                    category: x["category"].as_str().unwrap_or("").to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn list(http: &reqwest::Client, key: &str) -> Result<Vec<Voice>> {
    let v = get_json(http, key, &format!("{API}/v2/voices?page_size=100")).await?;
    Ok(parse_voices(&v))
}

/// Accepts a voice ID or a (prefix of a) voice name on the account, case-insensitively.
pub async fn resolve(http: &reqwest::Client, key: &str, wanted: &str) -> Result<Voice> {
    let voices = list(http, key).await?;
    if let Some(v) = voices.iter().find(|v| v.id == wanted) {
        return Ok(Voice { id: v.id.clone(), name: v.name.clone(), category: v.category.clone() });
    }
    let w = wanted.to_lowercase();
    let short = |name: &str| name.split(" - ").next().unwrap_or(name).trim().to_lowercase();
    let found = voices
        .iter()
        .find(|v| v.name.to_lowercase() == w)
        .or_else(|| voices.iter().find(|v| short(&v.name) == w))
        .or_else(|| voices.iter().find(|v| v.name.to_lowercase().starts_with(&w)));
    match found {
        Some(v) => Ok(Voice { id: v.id.clone(), name: v.name.clone(), category: v.category.clone() }),
        None if wanted.len() == 20 && wanted.chars().all(|c| c.is_ascii_alphanumeric()) => {
            Ok(Voice { id: wanted.to_string(), name: wanted.to_string(), category: String::new() })
        }
        None => Err(anyhow!("голосу «{wanted}» на акаунті немає; список: live-interpreter voices")),
    }
}

/// Instant Voice Cloning: uploads samples and returns the new voice ID.
pub async fn clone(
    http: &reqwest::Client,
    key: &str,
    name: &str,
    files: &[PathBuf],
    remove_noise: bool,
    description: Option<&str>,
) -> Result<(String, bool)> {
    let mut form = reqwest::multipart::Form::new()
        .text("name", name.to_string())
        .text("remove_background_noise", remove_noise.to_string());
    if let Some(d) = description {
        form = form.text("description", d.to_string());
    }
    for path in files {
        let data = std::fs::read(path).with_context(|| format!("не вдалося прочитати {}", path.display()))?;
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("sample.wav").to_string();
        let mime = match path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref() {
            Some("mp3") => "audio/mpeg",
            Some("m4a") => "audio/mp4",
            Some("ogg") => "audio/ogg",
            Some("flac") => "audio/flac",
            _ => "audio/wav",
        };
        form = form.part("files", reqwest::multipart::Part::bytes(data).file_name(file_name).mime_str(mime)?);
    }
    let resp = http.post(format!("{API}/v1/voices/add")).header("xi-api-key", key).multipart(form).send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        bail!("ElevenLabs {status}: {text}");
    }
    let v: Value = serde_json::from_str(&text)?;
    let id = v["voice_id"].as_str().ok_or_else(|| anyhow!("у відповіді немає voice_id: {text}"))?;
    Ok((id.to_string(), v["requires_verification"].as_bool().unwrap_or(false)))
}

/// Records the microphone to a mono 16-bit WAV, showing a level meter. Ctrl+C stops early.
pub async fn record(path: &Path, seconds: u32, device: Option<&str>) -> Result<()> {
    let (mic, mut rx) = Mic::start(device)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec { channels: 1, sample_rate: mic.rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec)?;
    println!("Мікрофон: {} ({} Гц). Запис {seconds} с, Ctrl+C — зупинити раніше.", mic.name, mic.rate);
    println!("Говори природно, як зазвичай, без пауз по кілька секунд. Тиша й шум погіршують клон.\n");

    let total = mic.rate as u64 * seconds as u64;
    let mut written = 0u64;
    let mut peak = 0f32;
    let mut sum_sq = 0f64;
    let mut n = 0u64;
    let mut last_draw = Instant::now();
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    while written < total {
        tokio::select! {
            block = rx.recv() => {
                let Some(block) = block else { break };
                for &s in block.iter().take((total - written) as usize) {
                    writer.write_sample(f32_to_i16(s))?;
                    peak = peak.max(s.abs());
                    sum_sq += (s as f64) * (s as f64);
                    n += 1;
                }
                written += block.len() as u64;
                if last_draw.elapsed() > Duration::from_millis(150) {
                    let rms = if n > 0 { (sum_sq / n as f64).sqrt() as f32 } else { 0.0 };
                    let db = 20.0 * rms.max(1e-5).log10();
                    let bars = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 30.0) as usize;
                    let left = (total.saturating_sub(written)) / mic.rate as u64;
                    let warn = if peak >= 0.99 { " ПЕРЕВАНТАЖЕННЯ: тихіше або далі від мікрофона" } else { "" };
                    print!("\r  [{:<30}] {:>4.0} дБ, лишилось {:>3} с{:<50}", "#".repeat(bars), db, left, warn);
                    use std::io::Write as _;
                    let _ = std::io::stdout().flush();
                    peak = 0.0; sum_sq = 0.0; n = 0;
                    last_draw = Instant::now();
                }
            }
            _ = &mut ctrl_c => break,
        }
    }
    writer.finalize()?;
    let secs = written.min(total) as f64 / mic.rate as f64;
    println!("\n\nЗбережено {:.0} с у {}", secs, path.display());
    Ok(())
}
