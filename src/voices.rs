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
        for (file_name, data, mime) in prepare_upload(path)? {
            form = form.part("files", reqwest::multipart::Part::bytes(data).file_name(file_name).mime_str(mime)?);
        }
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
/// A passage to read aloud while recording: questions, exclamations, numbers and calm narration,
/// so the clone hears the voice in different intonations.
const READING_TEXT: &str = "\
Привіт! Зараз я записую свій голос, щоб перевірити, як працює клонування. День сьогодні звичайний:
зранку я випив каву, переглянув новини й трохи попрацював. Чи знаєте ви, скільки мов існує у світі?
Кажуть, понад сім тисяч. Я розмовляю українською, а мій цифровий двійник, можливо, скоро заговорить
японською. Це звучить трохи дивно, але водночас дуже цікаво!

Уявіть, що ви вперше приїхали в Токіо. Довкола яскраві вивіски, люди поспішають у метро, а ви шукаєте
потрібну станцію. Ви підходите до перехожого й питаєте дорогу, а телефон одразу перекладає ваші слова
вашим же голосом. Колись це здавалося фантастикою, а тепер це звичайна програма на комп'ютері.

Тепер кілька чисел: один, два, три, двадцять п'ять, сто сорок сім. Двадцять перше листопада, о пів на
восьму вечора.

Наостанок скажу щось спокійно й повільно. Добре, коли технології допомагають людям розуміти одне
одного. Дякую, що вислухали!";

async fn wait_for_enter() -> Result<()> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).map(|_| ())
    })
    .await??;
    Ok(())
}

/// Raises a quiet sample towards -20 dB RMS of speech (ElevenLabs recommends -23..-18 dB) without
/// pushing peaks above -3 dB. Never lowers the level. Returns the gain applied, in dB.
pub(crate) fn normalize_speech(samples: &mut [f32], rate: u32) -> f32 {
    let window = (rate / 20) as usize; // 50 ms
    let (mut speech_sq, mut speech_n) = (0f64, 0usize);
    for w in samples.chunks(window) {
        let sq: f64 = w.iter().map(|&s| (s as f64) * (s as f64)).sum();
        if 10.0 * (sq / w.len() as f64).max(1e-12).log10() >= -45.0 {
            speech_sq += sq;
            speech_n += w.len();
        }
    }
    let peak = samples.iter().fold(0f32, |m, &s| m.max(s.abs()));
    if speech_n == 0 || peak <= 0.0 {
        return 0.0;
    }
    let speech_db = 10.0 * (speech_sq / speech_n as f64).log10() as f32;
    let peak_db = 20.0 * peak.log10();
    let gain_db = (-20.0 - speech_db).min(-3.0 - peak_db).max(0.0);
    let gain = 10f32.powf(gain_db / 20.0);
    for s in samples.iter_mut() {
        *s *= gain;
    }
    gain_db
}

/// ElevenLabs rejects uploads over 11 MB. WAV samples are converted to 24 kHz mono 16-bit, which
/// is plenty for cloning (two minutes take 5.8 MB), and split into parts when still too large.
/// Other formats are sent as they are.
pub(crate) fn prepare_upload(path: &Path) -> Result<Vec<(String, Vec<u8>, &'static str)>> {
    const UPLOAD_RATE: u32 = 24_000;
    const MAX_BYTES: usize = 10 * 1024 * 1024;
    let stem = path.file_stem().and_then(|n| n.to_str()).unwrap_or("sample").to_string();
    let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).unwrap_or_default();
    if ext != "wav" {
        let data = std::fs::read(path).with_context(|| format!("не вдалося прочитати {}", path.display()))?;
        if data.len() > MAX_BYTES {
            bail!("{} більший за 10 МБ; збережи його як WAV, і програма сама його зменшить", path.display());
        }
        let mime = match ext.as_str() {
            "mp3" => "audio/mpeg",
            "m4a" => "audio/mp4",
            "ogg" => "audio/ogg",
            "flac" => "audio/flac",
            _ => "application/octet-stream",
        };
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("sample").to_string();
        return Ok(vec![(name, data, mime)]);
    }

    let (samples, rate) = crate::audio::read_audio_file(path)?;
    let mut converted = Vec::new();
    crate::audio::Resampler::new(rate, UPLOAD_RATE).process(&samples, &mut converted);
    let gain_db = normalize_speech(&mut converted, UPLOAD_RATE);
    if gain_db > 0.5 {
        println!("{}: запис тихий, підсилюю на {gain_db:.0} дБ перед завантаженням.", path.display());
    }
    let per_part = (MAX_BYTES - 1024) / 2; // samples per part, leaving room for the WAV header
    let parts: Vec<&[f32]> = converted.chunks(per_part).collect();
    let spec = hound::WavSpec { channels: 1, sample_rate: UPLOAD_RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut out = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
            for &s in part.iter() {
                writer.write_sample(f32_to_i16(s))?;
            }
            writer.finalize()?;
        }
        let name = if parts.len() == 1 { format!("{stem}.wav") } else { format!("{stem}-{}.wav", i + 1) };
        out.push((name, cursor.into_inner(), "audio/wav"));
    }
    Ok(out)
}

/// Records the microphone to a mono 16-bit WAV, showing a level meter. Ctrl+C stops early.
/// Afterwards it reports level, clipping and pauses, which decide how good the clone will be.
pub async fn record(path: &Path, seconds: u32, device: Option<&str>) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    println!("Запис зразка голосу для клону: {seconds} с, файл {}.\n", path.display());
    println!("Говори природно, своїм звичайним тоном, у тихій кімнаті, без музики й телевізора.");
    println!("Сядь приблизно за 20 см від мікрофона. Під час мовлення рівень має бути від −23 до −18 дБ.");
    println!("Клон копіює все, що чує: темп, вагання, дихання й шум, тож говори рівно й без «е-е».");
    println!("Можна прочитати текст нижче, а коли він закінчиться, просто говорити про будь-що.\n");
    println!("{READING_TEXT}\n");
    println!("Натисни Enter, щоб почати запис (Ctrl+C — скасувати).");
    wait_for_enter().await?;

    let (mic, mut rx) = Mic::start(device)?;
    let spec = hound::WavSpec { channels: 1, sample_rate: mic.rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut writer = hound::WavWriter::create(path, spec)?;
    println!("Пишу з мікрофона «{}» ({} Гц). Ctrl+C зупинить запис раніше.\n", mic.name, mic.rate);

    let total = mic.rate as u64 * seconds as u64;
    let window = (mic.rate / 20) as u64; // 50 ms, for the pause statistics
    let mut written = 0u64;
    let (mut peak, mut sum_sq, mut n) = (0f32, 0f64, 0u64); // for the live meter
    let (mut peak_all, mut clipped) = (0f32, 0u64);
    let (mut win_sq, mut win_n) = (0f64, 0u64);
    let (mut speech_sq, mut speech_n, mut windows, mut silent_windows) = (0f64, 0u64, 0u64, 0u64);
    let mut last_draw = Instant::now();
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    while written < total {
        tokio::select! {
            block = rx.recv() => {
                let Some(block) = block else { break };
                for &s in block.iter().take((total - written) as usize) {
                    writer.write_sample(f32_to_i16(s))?;
                    let sq = (s as f64) * (s as f64);
                    peak = peak.max(s.abs());
                    peak_all = peak_all.max(s.abs());
                    if s.abs() >= 0.99 { clipped += 1; }
                    sum_sq += sq; n += 1;
                    win_sq += sq; win_n += 1;
                    if win_n == window {
                        windows += 1;
                        let db = 10.0 * (win_sq / win_n as f64).max(1e-12).log10();
                        if db < -45.0 { silent_windows += 1; } else { speech_sq += win_sq; speech_n += win_n; }
                        win_sq = 0.0; win_n = 0;
                    }
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
    let speech_db = if speech_n > 0 { 10.0 * (speech_sq / speech_n as f64).log10() } else { -99.0 };
    let pauses = if windows > 0 { silent_windows as f64 / windows as f64 * 100.0 } else { 100.0 };
    println!("\n\nЗбережено {secs:.0} с у {}", path.display());
    println!("Гучність мовлення: {speech_db:.0} дБ, пік: {:.0} дБ, тиша: {pauses:.0}% запису.", 20.0 * peak_all.max(1e-5).log10());
    let mut ok = true;
    if secs < 45.0 {
        ok = false;
        println!("  ! Запис коротший за 45 с. Для клону краще 1-2 хвилини мовлення.");
    }
    if clipped > mic.rate as u64 / 10 {
        ok = false;
        println!("  ! Звук часто перевантажений, клон може хрипіти. Говори тихіше або відсунь мікрофон.");
    }
    // ElevenLabs recommends -23..-18 dB RMS with peaks around -3 dB for cloning samples.
    let peak_db = 20.0 * peak_all.max(1e-5).log10();
    if speech_db < -26.0 {
        ok = false;
        println!("  ! Мовлення тихе: ElevenLabs радить від −23 до −18 дБ. Сядь ближче до мікрофона");
        println!("    (приблизно 20 см) або збільш його гучність і запиши ще раз. Якщо не перезаписувати,");
        println!("    clone сам підсилить запис, наскільки дозволяє запас до перевантаження.");
    } else if speech_db > -14.0 {
        ok = false;
        println!("  ! Мовлення дуже гучне: ElevenLabs радить від −23 до −18 дБ. Відсунь мікрофон або зменш гучність.");
    }
    if clipped <= mic.rate as u64 / 10 && peak_db > -1.0 {
        println!("  ! Піки майже доходять до перевантаження. ElevenLabs радить тримати їх нижче −3 дБ.");
    }
    if pauses > 40.0 {
        ok = false;
        println!("  ! Тиша займає понад 40% запису. Довгі паузи краще вирізати або перезаписати.");
    }
    if ok {
        println!("Запис придатний для клону.");
    }
    println!("\nДалі: live-interpreter clone --name \"Мій голос\" {}", path.display());
    Ok(())
}
