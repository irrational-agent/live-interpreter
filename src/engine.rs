//! The interpreting pipeline: microphone (or a file) -> Scribe realtime -> Claude -> ElevenLabs
//! speech on every chosen output, plus furigana and the VRChat chatbox. It reports what happens
//! through `Event`s, which the window shows as a transcript and the command line prints.

use crate::audio::{self, Mic, OutputChoice, Player, PlayerHandle, Resampler, f32_to_i16};
use crate::ruby::{self, Furigana, Ruby};
use crate::settings::Settings;
use crate::{osc, stt, translate, tts, voices};
use anyhow::{Context, Result, anyhow};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

pub struct Keys {
    pub eleven: String,
    pub anthropic: Option<String>,
}

/// Loads `.env.local` from the working directory or from the project folder above the executable.
pub fn load_keys() -> Result<Keys> {
    if let Some(dir) = crate::settings::project_dir() {
        let file = dir.join(".env.local");
        dotenvy::from_path(&file).with_context(|| format!("не вдалося прочитати {}", file.display()))?;
    }
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let eleven = get("ELEVENLABS_API_KEY")
        .ok_or_else(|| anyhow!("немає ключа ElevenLabs: запусти scripts\\set-key.ps1 у теці проекту"))?;
    Ok(Keys { eleven, anthropic: get("ANTHROPIC_API_KEY") })
}

pub fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?)
}

#[derive(Clone, Debug)]
pub enum Event {
    Started { source: String, voice: String, outputs: Vec<String> },
    /// The phrase being spoken right now, as recognized so far.
    Partial(String),
    /// A finished phrase; `id` ties the later events to it.
    Segment { id: u64, text: String },
    /// `to` is the language of this translation: the target can change while the engine runs.
    Translated { id: u64, text: String, to: String, claude: Option<Duration>, model: String },
    Furigana { id: u64, parts: Vec<Ruby> },
    /// Speech for the phrase started this long after the phrase was finished.
    Spoken { id: u64, after: Duration },
    /// Something went wrong with one phrase; the engine keeps running.
    Problem(String),
}

pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;

pub struct Config {
    pub settings: Settings,
    /// Take speech from a WAV or 16 kHz `.pcm` file instead of the microphone.
    pub input_file: Option<PathBuf>,
    pub save_output: Option<PathBuf>,
    /// Play nothing (for tests).
    pub mute: bool,
    /// Lets the window change the target language while the engine runs; the next phrase
    /// is translated into the new language without reconnecting anything.
    pub target: Option<watch::Receiver<String>>,
}

/// The outputs the settings ask for, in order: listening output first, then the virtual mic.
pub fn output_choices(settings: &Settings, mute: bool) -> Vec<OutputChoice> {
    let mut choices = Vec::new();
    if mute {
        return choices;
    }
    if settings.monitor {
        choices.push(settings.monitor_device.clone());
    }
    if let Some(mic) = &settings.mic_output {
        choices.push(Some(mic.clone()));
    }
    choices
}

/// True when a segment is only hesitation sounds ("А-а-а.", "Ммм", "uh, um"): nothing to translate.
pub fn is_filler_only(text: &str) -> bool {
    const FILLERS: &[&str] = &[
        "а", "е", "є", "о", "у", "и", "і", "м", "ам", "ем", "хм", "ну", "uh", "um", "er", "erm", "ah", "eh", "oh",
        "hm", "m", "a", "e", "o",
    ];
    let mut words = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).peekable();
    if words.peek().is_none() {
        return true;
    }
    words.all(|w| {
        // Collapse stretched sounds: "ааа" -> "а", "мммм" -> "м", "ееем" -> "ем".
        let mut squeezed = String::new();
        for c in w.to_lowercase().chars() {
            if !squeezed.ends_with(c) {
                squeezed.push(c);
            }
        }
        FILLERS.contains(&squeezed.as_str())
    })
}

struct Utterance {
    id: u64,
    text: String,
    language: String,
    committed_at: Instant,
    first_of_segment: bool,
}

/// Runs until `stop` turns true, or until an input file has been fully interpreted.
pub async fn run(cfg: Config, keys: Keys, emit: Emit, mut stop: watch::Receiver<bool>) -> Result<()> {
    let s = cfg.settings.clone();
    let anthropic = keys
        .anthropic
        .clone()
        .ok_or_else(|| anyhow!("немає ключа Claude: запусти scripts\\set-key.ps1 -Name ANTHROPIC_API_KEY"))?;
    let http = http_client()?;
    let player = Player::start(&output_choices(&s, cfg.mute))?;
    let voice = voices::resolve(&http, &keys.eleven, &s.voice).await?;
    let save = cfg.save_output.as_deref().map(tts::wav_sink).transpose()?;

    let stt_opts = stt::SttOptions { language: s.from.clone(), silence_secs: s.silence, turn_prediction: s.turn_prediction };
    let (audio_tx, mut events) = stt::connect(&keys.eleven, &stt_opts).await?;

    let (_mic, src_rate, src_rx, source) = match &cfg.input_file {
        Some(path) => {
            let (samples, rate) = audio::read_audio_file(path)?;
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                let block = (rate / 10) as usize;
                for chunk in samples.chunks(block) {
                    if tx.send(chunk.to_vec()).is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                // Trailing silence so the last phrase gets committed.
                for _ in 0..30 {
                    if tx.send(vec![0.0; block]).is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            });
            (None, rate, rx, format!("файл {}", path.display()))
        }
        None => {
            let (mic, rx) = Mic::start(s.input_device.as_deref())?;
            let (rate, name) = (mic.rate, mic.name.clone());
            (Some(mic), rate, rx, name)
        }
    };

    let echo_guard = cfg.input_file.is_none() && s.echo_guard;
    tokio::spawn(feed_audio(src_rx, src_rate, audio_tx, player.handle(), echo_guard));
    emit(Event::Started { source, voice: voice.name.clone(), outputs: player.names.clone() });

    let chatbox = if s.vrchat {
        let (tx, rx) = mpsc::unbounded_channel::<ChatboxMessage>();
        let chatbox = osc::Chatbox::new(&s.vrchat_address)?;
        tokio::spawn(chatbox_loop(chatbox, rx, emit.clone()));
        Some(tx)
    } else {
        None
    };
    let furigana = s.furigana.then(|| Furigana::new(http.clone(), anthropic.clone(), s.model.clone()));

    let (seg_tx, seg_rx) = mpsc::unbounded_channel::<(u64, String, Instant)>();
    let (say_tx, say_rx) = mpsc::unbounded_channel::<Utterance>();
    let translator = translate::Translator::new(http.clone(), anthropic, s.model.clone(), s.effort.clone(), &s.from, &s.to);
    let extras = Extras {
        emit: emit.clone(),
        furigana,
        chatbox: chatbox.clone(),
        chatbox_original: s.vrchat_original,
        target: cfg.target.clone(),
    };
    let translate_task = tokio::spawn(translate_loop(translator, seg_rx, say_tx, extras));
    let speaker = tts::Speaker::new(http, keys.eleven.clone(), voice.id.clone(), s.tts_model.clone(), s.to.clone(), save.clone());
    let speak_task = tokio::spawn(speak_loop(speaker, say_rx, player.handle(), emit.clone()));

    let mut next_id = 0u64;
    let mut interrupted = false;
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(stt::SttEvent::Started) => {}
                // Scribe sometimes "hears" a stray word like "Да." in silence; hide such scraps.
                Some(stt::SttEvent::Partial(text)) => {
                    if text.chars().filter(|c| c.is_alphabetic()).count() >= 3 {
                        emit(Event::Partial(text));
                    }
                }
                Some(stt::SttEvent::Committed(text)) => {
                    emit(Event::Partial(String::new()));
                    let text = text.trim().to_string();
                    if text.chars().filter(|c| c.is_alphanumeric()).count() >= 2 && !is_filler_only(&text) {
                        next_id += 1;
                        emit(Event::Segment { id: next_id, text: text.clone() });
                        if let Some(c) = &chatbox {
                            let _ = c.send(ChatboxMessage::Typing);
                        }
                        let _ = seg_tx.send((next_id, text, Instant::now()));
                    }
                }
                Some(stt::SttEvent::Error(e)) => emit(Event::Problem(format!("Scribe: {e}"))),
                Some(stt::SttEvent::Closed(reason)) => {
                    if cfg.input_file.is_none() {
                        return Err(anyhow!("Scribe: {reason}"));
                    }
                    break;
                }
                None => break,
            },
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    interrupted = true;
                    break;
                }
            }
        }
    }

    drop(seg_tx);
    if !interrupted {
        // Let the last phrases finish translating and speaking.
        let _ = translate_task.await;
        let _ = speak_task.await;
        player.handle().wait_idle().await;
    } else {
        translate_task.abort();
        speak_task.abort();
    }
    if let Some(c) = &chatbox {
        let _ = c.send(ChatboxMessage::StopTyping);
    }
    if let Some(s) = &save {
        tts::finish_wav(s)?;
    }
    Ok(())
}

/// Resamples source audio to 16 kHz, cuts it into 100 ms blocks and sends it to Scribe.
async fn feed_audio(
    mut src: mpsc::UnboundedReceiver<Vec<f32>>,
    src_rate: u32,
    out: mpsc::Sender<Vec<i16>>,
    player: PlayerHandle,
    echo_guard: bool,
) {
    let mut resampler = Resampler::new(src_rate, stt::SAMPLE_RATE);
    let mut buf: Vec<f32> = Vec::new();
    let block = (stt::SAMPLE_RATE / 10) as usize;
    while let Some(samples) = src.recv().await {
        resampler.process(&samples, &mut buf);
        while buf.len() >= block {
            let speaking = echo_guard && player.busy(Duration::from_millis(400));
            let pcm: Vec<i16> = buf.drain(..block).map(|s| if speaking { 0 } else { f32_to_i16(s) }).collect();
            if out.send(pcm).await.is_err() {
                return;
            }
        }
    }
}

struct Extras {
    emit: Emit,
    furigana: Option<Furigana>,
    chatbox: Option<mpsc::UnboundedSender<ChatboxMessage>>,
    chatbox_original: bool,
    target: Option<watch::Receiver<String>>,
}

async fn translate_loop(
    mut tr: translate::Translator,
    mut segments: mpsc::UnboundedReceiver<(u64, String, Instant)>,
    say: mpsc::UnboundedSender<Utterance>,
    extras: Extras,
) {
    fn send(say: &mpsc::UnboundedSender<Utterance>, id: u64, text: String, language: &str, at: Instant, first: &mut bool) {
        let _ = say.send(Utterance { id, text, language: language.to_string(), committed_at: at, first_of_segment: *first });
        *first = false;
    }
    let mut readings = tokio::task::JoinSet::new();
    while let Some((id, segment, committed_at)) = segments.recv().await {
        if let Some(target) = &extras.target {
            let to = target.borrow().clone();
            tr.set_target(&to);
        }
        let language = tr.target().to_string();
        let mut splitter = translate::SentenceSplitter::default();
        let mut first = true;
        // Each finished sentence goes to speech while Claude is still writing the next one.
        let result = tr
            .translate(&segment, |piece| {
                for sentence in splitter.push(piece) {
                    send(&say, id, sentence, &language, committed_at, &mut first);
                }
            })
            .await;
        match result {
            Ok(t) => {
                if let Some(rest) = splitter.finish() {
                    send(&say, id, rest, &language, committed_at, &mut first);
                }
                if t.text.is_empty() {
                    continue;
                }
                (extras.emit)(Event::Translated {
                    id,
                    text: t.text.clone(),
                    to: language.clone(),
                    claude: t.first_token,
                    model: t.model,
                });
                if let Some(chatbox) = &extras.chatbox {
                    let text = if extras.chatbox_original { format!("{segment}\n{}", t.text) } else { t.text.clone() };
                    let _ = chatbox.send(ChatboxMessage::Text(text));
                }
                let wants_readings = ruby::supports(&language) && ruby::has_kanji(&t.text);
                if let Some(furigana) = extras.furigana.clone().filter(|_| wants_readings) {
                    let emit = extras.emit.clone();
                    let language = language.clone();
                    readings.spawn(async move {
                        match furigana.annotate(&t.text, &language).await {
                            Ok(parts) => emit(Event::Furigana { id, parts }),
                            Err(e) => eprintln!("читання: {e:#}"),
                        }
                    });
                }
            }
            Err(e) => (extras.emit)(Event::Problem(format!("переклад не вдався: {e:#}"))),
        }
        while readings.try_join_next().is_some() {}
    }
    // The input ended: let the readings of the last phrases arrive before the engine shuts down.
    let _ = tokio::time::timeout(Duration::from_secs(8), async { while readings.join_next().await.is_some() {} }).await;
}

async fn speak_loop(mut speaker: tts::Speaker, mut said: mpsc::UnboundedReceiver<Utterance>, player: PlayerHandle, emit: Emit) {
    while let Some(u) = said.recv().await {
        speaker.set_language(&u.language);
        match speaker.speak(&u.text, &player).await {
            Ok(_) if u.first_of_segment => emit(Event::Spoken { id: u.id, after: u.committed_at.elapsed() }),
            Ok(_) => {}
            Err(e) => emit(Event::Problem(format!("озвучення не вдалося: {e:#}"))),
        }
    }
}

enum ChatboxMessage {
    Typing,
    StopTyping,
    Text(String),
}

/// VRChat drops chatbox messages that come too often, so they go out at least this far apart.
const CHATBOX_GAP: Duration = Duration::from_millis(1600);

/// Sends translations to the VRChat chatbox no faster than it accepts them. When several wait,
/// they are joined, dropping the oldest ones that no longer fit.
async fn chatbox_loop(chatbox: osc::Chatbox, mut rx: mpsc::UnboundedReceiver<ChatboxMessage>, emit: Emit) {
    let mut last_sent: Option<Instant> = None;
    let mut pending: Vec<String> = Vec::new();
    loop {
        let message = if pending.is_empty() {
            rx.recv().await
        } else {
            let wait = last_sent.map(|t| CHATBOX_GAP.saturating_sub(t.elapsed())).unwrap_or_default();
            tokio::select! {
                m = rx.recv() => m,
                _ = tokio::time::sleep(wait) => {
                    let mut joined: Vec<&str> = pending.iter().map(String::as_str).collect();
                    while joined.len() > 1 && joined.join(" ").chars().count() > osc::MAX_CHARS {
                        joined.remove(0);
                    }
                    if let Err(e) = chatbox.send(&joined.join(" ")) {
                        emit(Event::Problem(format!("VRChat OSC: {e}")));
                    }
                    let _ = chatbox.typing(false);
                    pending.clear();
                    last_sent = Some(Instant::now());
                    continue;
                }
            }
        };
        match message {
            None => return,
            Some(ChatboxMessage::Typing) => {
                let _ = chatbox.typing(true);
            }
            Some(ChatboxMessage::StopTyping) => {
                let _ = chatbox.typing(false);
            }
            Some(ChatboxMessage::Text(text)) => pending.push(text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filler_detection() {
        assert!(is_filler_only("А-а-а."));
        assert!(is_filler_only("Ммм..."));
        assert!(is_filler_only("Ееее, ну"));
        assert!(!is_filler_only("Так, а-а-а..."));
        assert!(!is_filler_only("Ну, це дивно."));
    }

    #[test]
    fn outputs_follow_settings() {
        let mut s = Settings::default();
        assert_eq!(output_choices(&s, false), vec![None]);
        s.mic_output = Some("Voicemeeter AUX Input".into());
        assert_eq!(output_choices(&s, false), vec![None, Some("Voicemeeter AUX Input".into())]);
        s.monitor = false;
        assert_eq!(output_choices(&s, false), vec![Some("Voicemeeter AUX Input".into())]);
        assert!(output_choices(&s, true).is_empty());
    }
}
