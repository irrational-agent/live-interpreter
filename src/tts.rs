//! ElevenLabs text-to-speech over HTTP streaming, played as the bytes arrive, and the choice
//! between that and the Eleven v4 Turbo WebSocket.

use crate::audio::{PlayerHandle, Resampler, f32_to_i16};
use anyhow::{Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const TTS_RATE: u32 = 24_000;

pub type WavSink = Arc<Mutex<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>>>;

pub fn wav_sink(path: &std::path::Path) -> Result<WavSink> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: TTS_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    Ok(Arc::new(Mutex::new(Some(hound::WavWriter::create(path, spec)?))))
}

pub fn finish_wav(sink: &WavSink) -> Result<()> {
    if let Some(w) = sink.lock().unwrap().take() {
        w.finalize()?;
    }
    Ok(())
}

/// Plays (and optionally saves) an even number of bytes of 24 kHz mono s16le PCM.
pub fn write_pcm(bytes: &[u8], resamplers: &mut [Resampler], player: &PlayerHandle, save: Option<&WavSink>) -> Result<()> {
    let samples: Vec<f32> = bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect();
    if let Some(sink) = save {
        if let Some(w) = sink.lock().unwrap().as_mut() {
            for &s in &samples {
                w.write_sample(f32_to_i16(s))?;
            }
        }
    }
    player.push(&samples, resamplers);
    Ok(())
}

pub struct Tts {
    http: reqwest::Client,
    api_key: String,
    voice_id: String,
    model: String,
    language: String,
    previous: Option<String>,
    save: Option<WavSink>,
}

impl Tts {
    pub fn new(
        http: reqwest::Client,
        api_key: String,
        voice_id: String,
        model: String,
        language: String,
        save: Option<WavSink>,
    ) -> Self {
        Self { http, api_key, voice_id, model, language, previous: None, save }
    }

    /// Speaks `text`; returns the time until the first audio bytes arrived.
    pub async fn speak(&mut self, text: &str, player: &PlayerHandle) -> Result<Duration> {
        let started = Instant::now();
        let mut body = json!({
            "text": text,
            "model_id": self.model,
            "language_code": self.language,
        });
        if let Some(prev) = &self.previous {
            body["previous_text"] = json!(prev);
        }
        let url = format!(
            "https://api.elevenlabs.io/v1/text-to-speech/{}/stream?output_format=pcm_{TTS_RATE}",
            self.voice_id
        );
        let resp = self.http.post(url).header("xi-api-key", &self.api_key).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .map(|v| v["detail"]["message"].as_str().map(str::to_string).unwrap_or_else(|| v["detail"].to_string()))
                .unwrap_or(text);
            bail!("ElevenLabs TTS {status}: {detail}");
        }

        let mut first_audio = None;
        let mut carry: Option<u8> = None;
        let mut resamplers = player.resamplers(TTS_RATE);
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if chunk.is_empty() {
                continue;
            }
            first_audio.get_or_insert_with(|| started.elapsed());
            let mut bytes = Vec::with_capacity(chunk.len() + 1);
            bytes.extend(carry.take());
            bytes.extend_from_slice(&chunk);
            if bytes.len() % 2 == 1 {
                carry = bytes.pop();
            }
            write_pcm(&bytes, &mut resamplers, player, self.save.as_ref())?;
        }
        self.previous = Some(text.to_string());
        Ok(first_audio.unwrap_or_else(|| started.elapsed()))
    }
}

/// Picks the transport by model: Eleven v4 Turbo streams over the dialogue WebSocket, every other
/// model over HTTP streaming.
pub enum Speaker {
    Http(Tts),
    Turbo(crate::tts_ws::TurboTts),
}

impl Speaker {
    pub fn new(
        http: reqwest::Client,
        api_key: String,
        voice_id: String,
        model: String,
        language: String,
        save: Option<WavSink>,
    ) -> Self {
        if model == "eleven_v4_turbo" {
            Speaker::Turbo(crate::tts_ws::TurboTts::new(api_key, voice_id, model, save))
        } else {
            Speaker::Http(Tts::new(http, api_key, voice_id, model, language, save))
        }
    }

    pub async fn speak(&mut self, text: &str, player: &PlayerHandle) -> Result<Duration> {
        match self {
            Speaker::Http(t) => t.speak(text, player).await,
            Speaker::Turbo(t) => t.speak(text, player).await,
        }
    }

    /// The HTTP models take a language hint; Eleven v4 Turbo reads the language from the text.
    pub fn set_language(&mut self, language: &str) {
        if let Speaker::Http(t) = self {
            if t.language != language {
                t.language = language.to_string();
                t.previous = None;
            }
        }
    }
}
