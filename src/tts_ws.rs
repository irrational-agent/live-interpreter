//! Eleven v4 Turbo text-to-speech over the Text to Dialogue WebSocket.
//!
//! Every sentence gets its own dialogue session: send the text, ask the server to close, and read
//! audio until `is_final`, so sentence boundaries stay clear. Opening a session costs a TLS and
//! WebSocket handshake, so a background task always keeps the next session open and registered,
//! pinging it before the server's 20 s inactivity timeout.

use crate::audio::PlayerHandle;
use crate::tts::{TTS_RATE, WavSink, write_pcm};
use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const KEEP_ALIVE_EVERY: Duration = Duration::from_secs(10);

struct Session {
    api_key: String,
    voice_id: String,
    url: String,
}

impl Session {
    async fn open(&self) -> Result<Ws> {
        let (mut ws, _) = match tokio_tungstenite::connect_async(self.url.as_str()).await {
            Ok(ok) => ok,
            Err(tokio_tungstenite::tungstenite::Error::Http(resp)) => {
                let body = resp.body().as_ref().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
                bail!("ElevenLabs відхилив WebSocket озвучення: HTTP {} {}", resp.status(), body);
            }
            Err(e) => return Err(e).context("не вдалося під'єднатися до WebSocket озвучення"),
        };
        // The first message authenticates and registers the one voice this session may use.
        let hello = json!({ "voices": [self.voice_id], "xi_api_key": self.api_key });
        ws.send(Message::Text(hello.to_string().into())).await?;
        Ok(ws)
    }
}

pub struct TurboTts {
    requests: mpsc::Sender<oneshot::Sender<Result<Ws>>>,
    voice_id: String,
    save: Option<WavSink>,
}

impl TurboTts {
    pub fn new(api_key: String, voice_id: String, model: String, save: Option<WavSink>) -> Self {
        let session = Session {
            api_key,
            voice_id: voice_id.clone(),
            url: format!(
                "wss://api.elevenlabs.io/v1/text-to-dialogue/stream-input?model_id={model}&output_format=pcm_{TTS_RATE}"
            ),
        };
        let (requests, rx) = mpsc::channel(1);
        tokio::spawn(keep_warm(session, rx));
        Self { requests, voice_id, save }
    }

    async fn take_session(&self) -> Result<Ws> {
        let (tx, rx) = oneshot::channel();
        self.requests.send(tx).await.map_err(|_| anyhow!("фонова задача озвучення зупинилась"))?;
        rx.await.map_err(|_| anyhow!("фонова задача озвучення зупинилась"))?
    }

    /// Speaks `text`; returns the time until the first audio arrived.
    pub async fn speak(&mut self, text: &str, player: &PlayerHandle) -> Result<Duration> {
        let started = Instant::now();
        let mut ws = self.take_session().await?;
        let inputs = json!({ "inputs": [{ "text": text, "voice_id": self.voice_id, "new_turn": false }] });
        let sent = async {
            ws.send(Message::Text(inputs.to_string().into())).await?;
            ws.send(Message::Text(json!({ "close_socket": true }).to_string().into())).await
        }
        .await;
        if sent.is_err() {
            // The ready session died (network blip, server timeout): open a fresh one once.
            ws = self.take_session().await?;
            ws.send(Message::Text(inputs.to_string().into())).await?;
            ws.send(Message::Text(json!({ "close_socket": true }).to_string().into())).await?;
        }

        let mut first_audio = None;
        let mut carry: Option<u8> = None;
        let mut resamplers = player.resamplers(TTS_RATE);
        while let Some(msg) = ws.next().await {
            let text = match msg {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => break,
                Ok(_) => continue,
                Err(e) => return Err(e).context("WebSocket озвучення обірвався"),
            };
            let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
            if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
                let detail = v["message"].as_str().map(str::to_string).unwrap_or_else(|| err.to_string());
                bail!("ElevenLabs озвучення: {detail}");
            }
            if let Some(audio) = v["audio"].as_str().filter(|a| !a.is_empty()) {
                first_audio.get_or_insert_with(|| started.elapsed());
                let mut bytes = Vec::new();
                bytes.extend(carry.take());
                bytes.extend(B64.decode(audio)?);
                if bytes.len() % 2 == 1 {
                    carry = bytes.pop();
                }
                write_pcm(&bytes, &mut resamplers, player, self.save.as_ref())?;
            }
            if v["is_final"].as_bool() == Some(true) {
                break;
            }
        }
        Ok(first_audio.unwrap_or_else(|| started.elapsed()))
    }
}

enum WarmEvent {
    Request(Option<oneshot::Sender<Result<Ws>>>),
    Tick,
    Incoming(bool),
}

/// Keeps one session open and pinged; hands it over on request and opens the next one.
/// It also reads the idle session, so WebSocket pings get answered and a closed session is noticed.
async fn keep_warm(session: Session, mut requests: mpsc::Receiver<oneshot::Sender<Result<Ws>>>) {
    let mut ready: Option<Ws> = None;
    let mut failures = 0u32;
    let mut tick = tokio::time::interval(KEEP_ALIVE_EVERY);
    loop {
        if ready.is_none() {
            match session.open().await {
                Ok(ws) => {
                    ready = Some(ws);
                    failures = 0;
                    tick.reset();
                }
                Err(e) => {
                    failures += 1;
                    // Hand the error to a waiting caller instead of retrying forever in silence.
                    if let Ok(reply) = requests.try_recv() {
                        let _ = reply.send(Err(e));
                    }
                    tokio::time::sleep(Duration::from_millis(500 * failures.min(10) as u64)).await;
                    continue;
                }
            }
        }
        let Some(ws) = ready.as_mut() else { continue };
        let event = tokio::select! {
            req = requests.recv() => WarmEvent::Request(req),
            _ = tick.tick() => WarmEvent::Tick,
            msg = ws.next() => WarmEvent::Incoming(matches!(msg, Some(Ok(m)) if !m.is_close())),
        };
        match event {
            WarmEvent::Request(None) => return,
            WarmEvent::Request(Some(reply)) => {
                if let Some(ws) = ready.take() {
                    let _ = reply.send(Ok(ws));
                }
            }
            WarmEvent::Tick => {
                let ping = json!({ "keep_alive": true }).to_string();
                if ws.send(Message::Text(ping.into())).await.is_err() {
                    ready = None;
                }
            }
            // Pings are answered by tungstenite while reading; stray messages are ignored.
            WarmEvent::Incoming(true) => {}
            WarmEvent::Incoming(false) => ready = None,
        }
    }
}
