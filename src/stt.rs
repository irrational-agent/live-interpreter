//! ElevenLabs Scribe realtime speech-to-text over a WebSocket.

use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

pub const SAMPLE_RATE: u32 = 16_000;

pub struct SttOptions {
    pub language: String,
    /// Seconds of silence before Scribe commits a segment.
    pub silence_secs: f32,
    /// Commit when the model predicts the speaker finished, instead of at the first pause.
    pub turn_prediction: bool,
}

pub enum SttEvent {
    Started,
    Partial(String),
    Committed(String),
    Error(String),
    Closed(String),
}

/// Connects and returns a sender for 16 kHz mono PCM blocks and a receiver of transcript events.
/// Dropping the sender closes the session after the server has processed what was sent.
pub async fn connect(
    api_key: &str,
    opts: &SttOptions,
) -> Result<(mpsc::Sender<Vec<i16>>, mpsc::UnboundedReceiver<SttEvent>)> {
    let strategy = if opts.turn_prediction { "turn_prediction" } else { "vad" };
    let url = format!(
        "wss://api.elevenlabs.io/v1/speech-to-text/realtime?model_id=scribe_v3_live\
         &audio_format=pcm_16000&commit_strategy={strategy}&language_code={lang}\
         &vad_silence_threshold_secs={silence}&no_verbatim=true&filter_background_audio=true",
        lang = opts.language,
        silence = opts.silence_secs,
    );
    let mut request = url.into_client_request()?;
    request.headers_mut().insert("xi-api-key", HeaderValue::from_str(api_key)?);

    let (ws, _) = match tokio_tungstenite::connect_async(request).await {
        Ok(ok) => ok,
        Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
            let body = resp.body().as_ref().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
            return Err(anyhow!("Scribe відхилив з'єднання: HTTP {} {}", resp.status(), body));
        }
        Err(e) => return Err(e).context("не вдалося під'єднатися до Scribe realtime"),
    };
    let (mut write, mut read) = ws.split();

    let (audio_tx, mut audio_rx) = mpsc::channel::<Vec<i16>>(512);
    let (event_tx, event_rx) = mpsc::unbounded_channel();

    tokio::spawn(async move {
        while let Some(block) = audio_rx.recv().await {
            let bytes: Vec<u8> = block.iter().flat_map(|s| s.to_le_bytes()).collect();
            let msg = json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": B64.encode(bytes),
                "commit": false,
                "sample_rate": SAMPLE_RATE,
            });
            if write.send(Message::Text(msg.to_string().into())).await.is_err() {
                return;
            }
        }
        let _ = write.send(Message::Close(None)).await;
    });

    let tx = event_tx.clone();
    tokio::spawn(async move {
        let mut reason = String::from("з'єднання закрито");
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(text)) => {
                    let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                    let kind = v["message_type"].as_str().unwrap_or("");
                    let text = v["text"].as_str().unwrap_or("").to_string();
                    let event = match kind {
                        "session_started" => SttEvent::Started,
                        "partial_transcript" => SttEvent::Partial(text),
                        "committed_transcript" => SttEvent::Committed(text),
                        "final_transcript"
                        | "final_transcript_with_timestamps"
                        | "committed_transcript_with_timestamps" => continue,
                        _ if v.get("error").is_some() || kind.contains("error") => {
                            let detail = v["error"].as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
                            SttEvent::Error(format!("{kind}: {detail}"))
                        }
                        _ => continue,
                    };
                    if tx.send(event).is_err() {
                        return;
                    }
                }
                Ok(Message::Close(frame)) => {
                    if let Some(f) = frame {
                        if !f.reason.is_empty() {
                            reason = format!("сервер закрив з'єднання: {} ({})", f.reason, f.code);
                        }
                    }
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    reason = format!("помилка з'єднання: {e}");
                    break;
                }
            }
        }
        let _ = tx.send(SttEvent::Closed(reason));
    });

    Ok((audio_tx, event_rx))
}
