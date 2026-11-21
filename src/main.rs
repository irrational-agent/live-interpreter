//! live-interpreter: speak one language, hear another.
//! Microphone -> ElevenLabs Scribe realtime (speech-to-text) -> Claude (translation)
//! -> ElevenLabs text-to-speech (optionally in your cloned voice) -> speakers.

mod audio;
mod stt;
mod translate;
mod tts;
mod voices;

use anyhow::{Context, Result, anyhow};
use audio::{Mic, Player, PlayerHandle, Resampler, f32_to_i16};
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Parser)]
#[command(name = "live-interpreter", version, about = "Синхронний перекладач: говориш однією мовою, чуєш іншу")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Слухати мікрофон і озвучувати переклад
    Run(RunArgs),
    /// Перекласти й озвучити один текст (перевірка голосу без мікрофона)
    Say(SayArgs),
    /// Записати зразок голосу для клонування
    Record(RecordArgs),
    /// Створити клон голосу (Instant Voice Cloning) із записаних зразків
    Clone(CloneArgs),
    /// Показати голоси акаунта ElevenLabs
    Voices,
    /// Показати мікрофони й виходи звуку
    Devices,
}

#[derive(Args, Clone)]
struct LangArgs {
    /// Мова, якою говориш (ISO 639-1)
    #[arg(long, default_value = "uk")]
    from: String,
    /// Мова перекладу (ISO 639-1)
    #[arg(long, default_value = "ja")]
    to: String,
    /// Модель Claude для перекладу
    #[arg(long, default_value = "claude-opus-5-5")]
    model: String,
    /// Зусилля Claude: low швидше, high ретельніше
    #[arg(long, default_value = "low")]
    effort: String,
}

#[derive(Args, Clone)]
struct VoiceArgs {
    /// Голос: назва з акаунта (зокрема твій клон) або voice_id
    #[arg(long, default_value = "George")]
    voice: String,
    /// Модель озвучення: eleven_flash_v2_5 (найшвидша) або eleven_v4 (найякісніша)
    #[arg(long, default_value = "eleven_flash_v2_5")]
    tts_model: String,
    /// Пристрій виходу звуку (частина назви)
    #[arg(long)]
    output_device: Option<String>,
    /// Зберегти все озвучене у WAV
    #[arg(long)]
    save_output: Option<PathBuf>,
    /// Не відтворювати звук (для перевірок)
    #[arg(long)]
    mute: bool,
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    lang: LangArgs,
    #[command(flatten)]
    voice: VoiceArgs,
    /// Мікрофон (частина назви)
    #[arg(long)]
    input_device: Option<String>,
    /// Брати мову з файлу (WAV або 16 кГц .pcm) замість мікрофона
    #[arg(long)]
    input_file: Option<PathBuf>,
    /// Ти в навушниках: слухати мікрофон і під час озвучення
    #[arg(long)]
    headphones: bool,
    /// Секунди тиші, після яких фраза вважається закінченою
    #[arg(long, default_value_t = 0.7)]
    silence: f32,
    /// Завершувати фразу за передбаченням кінця думки, а не за першою паузою
    #[arg(long)]
    turn_prediction: bool,
}

#[derive(Args)]
struct SayArgs {
    /// Текст мовою --from
    text: String,
    #[command(flatten)]
    lang: LangArgs,
    #[command(flatten)]
    voice: VoiceArgs,
    /// Озвучити текст як є, без перекладу
    #[arg(long)]
    no_translate: bool,
}

#[derive(Args)]
struct RecordArgs {
    /// Куди зберегти запис
    #[arg(default_value = "samples/voice.wav")]
    out: PathBuf,
    /// Тривалість у секундах (для клону досить 60-120)
    #[arg(long, default_value_t = 90)]
    seconds: u32,
    /// Мікрофон (частина назви)
    #[arg(long)]
    input_device: Option<String>,
}

#[derive(Args)]
struct CloneArgs {
    /// Назва нового голосу на акаунті
    #[arg(long)]
    name: String,
    /// Файли зі зразками голосу
    #[arg(required = true)]
    files: Vec<PathBuf>,
    /// Прибрати фоновий шум зі зразків
    #[arg(long)]
    remove_noise: bool,
    /// Опис голосу
    #[arg(long)]
    description: Option<String>,
}

/// Console output shared by all tasks: one live line for the partial transcript, and full
/// lines printed above it without tearing it apart.
mod ui {
    use std::io::Write as _;
    use std::sync::Mutex;

    static PARTIAL: Mutex<String> = Mutex::new(String::new());

    fn erase(shown: &str) {
        if !shown.is_empty() {
            print!("\r{}\r", " ".repeat(shown.chars().count()));
        }
    }

    pub fn partial(text: &str) {
        let mut shown = PARTIAL.lock().unwrap();
        erase(&shown);
        let chars: Vec<char> = text.trim().chars().collect();
        let tail: String = chars[chars.len().saturating_sub(70)..].iter().collect();
        *shown = if tail.is_empty() { String::new() } else { format!("  … {tail}") };
        print!("{shown}");
        let _ = std::io::stdout().flush();
    }

    pub fn clear_partial() {
        let mut shown = PARTIAL.lock().unwrap();
        erase(&shown);
        shown.clear();
        let _ = std::io::stdout().flush();
    }

    pub fn line(text: &str) {
        let shown = PARTIAL.lock().unwrap();
        erase(&shown);
        println!("{text}");
        print!("{shown}");
        let _ = std::io::stdout().flush();
    }
}

struct Keys {
    eleven: String,
    anthropic: Option<String>,
}

/// Loads `.env.local` from the working directory or from the project folder above the executable.
fn load_keys() -> Result<Keys> {
    let mut dirs: Vec<PathBuf> = std::env::current_dir().into_iter().collect();
    if let Ok(exe) = std::env::current_exe() {
        dirs.extend(exe.ancestors().skip(1).map(PathBuf::from));
    }
    if let Some(file) = dirs.iter().map(|d| d.join(".env.local")).find(|p| p.is_file()) {
        dotenvy::from_path(&file).with_context(|| format!("не вдалося прочитати {}", file.display()))?;
    }
    let get = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let eleven = get("ELEVENLABS_API_KEY").ok_or_else(|| {
        anyhow!("немає ELEVENLABS_API_KEY: запусти scripts\\set-key.ps1 у теці проекту")
    })?;
    Ok(Keys { eleven, anthropic: get("ANTHROPIC_API_KEY") })
}

fn http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder().connect_timeout(Duration::from_secs(10)).build()?)
}

fn secs(d: Duration) -> String {
    format!("{:.1} с", d.as_secs_f32())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(e) = real_main(cli).await {
        eprintln!("\nПомилка: {e:#}");
        std::process::exit(1);
    }
}

async fn real_main(cli: Cli) -> Result<()> {
    match cli.cmd {
        Cmd::Devices => audio::list_devices(),
        Cmd::Record(a) => voices::record(&a.out, a.seconds, a.input_device.as_deref()).await,
        Cmd::Voices => {
            let keys = load_keys()?;
            let list = voices::list(&http_client()?, &keys.eleven).await?;
            for v in list {
                println!("{:<12} {}  {}", v.category, v.id, v.name);
            }
            Ok(())
        }
        Cmd::Clone(a) => {
            let keys = load_keys()?;
            println!("Завантажую {} файл(и) у ElevenLabs...", a.files.len());
            let (id, verify) =
                voices::clone(&http_client()?, &keys.eleven, &a.name, &a.files, a.remove_noise, a.description.as_deref())
                    .await?;
            println!("Готово: голос «{}», voice_id {id}", a.name);
            if verify {
                println!("ElevenLabs просить підтвердити голос: зайди в розділ Voices на elevenlabs.io.");
            }
            println!("\nСпробувати: live-interpreter say \"Привіт, це мій голос японською\" --voice \"{}\"", a.name);
            Ok(())
        }
        Cmd::Say(a) => say(a).await,
        Cmd::Run(a) => run(a).await,
    }
}

async fn say(a: SayArgs) -> Result<()> {
    let keys = load_keys()?;
    let http = http_client()?;
    let player = Player::start(a.voice.output_device.as_deref(), a.voice.mute)?;
    let voice = voices::resolve(&http, &keys.eleven, &a.voice.voice).await?;
    let save = a.voice.save_output.as_deref().map(tts::wav_sink).transpose()?;
    let (text, lang) = if a.no_translate {
        (a.text.clone(), a.lang.from.clone())
    } else {
        let key = keys.anthropic.clone().ok_or_else(|| anyhow!("немає ANTHROPIC_API_KEY для перекладу"))?;
        let mut tr = translate::Translator::new(http.clone(), key, a.lang.model.clone(), a.lang.effort.clone(), &a.lang.from, &a.lang.to);
        let t = tr.translate(&a.text, |_| {}).await?;
        println!("{} ◂ {}   [Claude {}]", a.lang.to, t.text, t.first_token.map(secs).unwrap_or_default());
        (t.text, a.lang.to.clone())
    };
    let mut speaker = tts::Tts::new(http, keys.eleven, voice.id.clone(), a.voice.tts_model.clone(), lang, save.clone());
    let first = speaker.speak(&text, &player.handle()).await?;
    println!("голос «{}», {} · звук пішов через {}", voice.name, a.voice.tts_model, secs(first));
    player.handle().wait_idle().await;
    if let Some(s) = &save {
        tts::finish_wav(s)?;
    }
    Ok(())
}

struct Utterance {
    text: String,
    /// When the source segment was committed, for latency reporting.
    committed_at: Instant,
    first_of_segment: bool,
}

async fn run(a: RunArgs) -> Result<()> {
    let keys = load_keys()?;
    let anthropic = keys.anthropic.clone().ok_or_else(|| anyhow!("немає ANTHROPIC_API_KEY: запусти scripts\\set-key.ps1 -Name ANTHROPIC_API_KEY"))?;
    let http = http_client()?;
    let player = Player::start(a.voice.output_device.as_deref(), a.voice.mute)?;
    let voice = voices::resolve(&http, &keys.eleven, &a.voice.voice).await?;
    let save = a.voice.save_output.as_deref().map(tts::wav_sink).transpose()?;

    let stt_opts = stt::SttOptions { language: a.lang.from.clone(), silence_secs: a.silence, turn_prediction: a.turn_prediction };
    let (audio_tx, mut events) = stt::connect(&keys.eleven, &stt_opts).await?;

    // Audio source: microphone or a file played in real time.
    let (_mic, src_rate, src_rx, source_label) = match &a.input_file {
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
            let (mic, rx) = Mic::start(a.input_device.as_deref())?;
            let rate = mic.rate;
            let label = format!("мікрофон «{}»", mic.name);
            (Some(mic), rate, rx, label)
        }
    };

    // Speakers + microphone: the mic would hear the translation and translate it again.
    let echo_guard = a.input_file.is_none() && !a.headphones && !a.voice.mute;
    tokio::spawn(feed_audio(src_rx, src_rate, audio_tx, player.handle(), echo_guard));

    println!("Слухаю: {source_label}. {} → {}", a.lang.from, a.lang.to);
    println!(
        "Переклад: {} (effort {}), голос: «{}», {}, вихід: {}",
        a.lang.model, a.lang.effort, voice.name, a.voice.tts_model, player.name
    );
    if echo_guard {
        println!("Поки звучить переклад, мікрофон не слухаю, щоб не перекладати самого себе. У навушниках додай --headphones.");
    }
    println!("Ctrl+C — вихід.\n");

    // Translation and speech run one after another in their own tasks, so listening never stops.
    let (seg_tx, seg_rx) = mpsc::unbounded_channel::<(String, Instant)>();
    let (say_tx, say_rx) = mpsc::unbounded_channel::<Utterance>();
    let translator = translate::Translator::new(http.clone(), anthropic, a.lang.model.clone(), a.lang.effort.clone(), &a.lang.from, &a.lang.to);
    let translate_task = tokio::spawn(translate_loop(translator, seg_rx, say_tx, a.lang.to.clone()));
    let speaker = tts::Tts::new(http, keys.eleven.clone(), voice.id.clone(), a.voice.tts_model.clone(), a.lang.to.clone(), save.clone());
    let speak_task = tokio::spawn(speak_loop(speaker, say_rx, player.handle()));

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut interrupted = false;
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(stt::SttEvent::Started) => {}
                Some(stt::SttEvent::Partial(text)) => ui::partial(&text),
                Some(stt::SttEvent::Committed(text)) => {
                    ui::clear_partial();
                    let text = text.trim().to_string();
                    if text.chars().filter(|c| c.is_alphanumeric()).count() >= 2 && !is_filler_only(&text) {
                        ui::line(&format!("{} ▸ {text}", a.lang.from));
                        let _ = seg_tx.send((text, Instant::now()));
                    }
                }
                Some(stt::SttEvent::Error(e)) => ui::line(&format!("Scribe: {e}")),
                Some(stt::SttEvent::Closed(reason)) => {
                    ui::clear_partial();
                    if a.input_file.is_none() {
                        ui::line(&format!("Scribe: {reason}"));
                    }
                    break;
                }
                None => break,
            },
            _ = &mut ctrl_c => { interrupted = true; break; }
        }
    }

    drop(seg_tx);
    if !interrupted {
        // Let the last phrases finish translating and speaking.
        let _ = translate_task.await;
        let _ = speak_task.await;
        player.handle().wait_idle().await;
    }
    if let Some(s) = &save {
        tts::finish_wav(s)?;
        println!("Озвучення збережено.");
    }
    Ok(())
}

/// True when a segment is only hesitation sounds ("А-а-а.", "Ммм", "uh, um"): nothing to translate.
fn is_filler_only(text: &str) -> bool {
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

async fn translate_loop(
    mut tr: translate::Translator,
    mut segments: mpsc::UnboundedReceiver<(String, Instant)>,
    say: mpsc::UnboundedSender<Utterance>,
    to: String,
) {
    fn emit(say: &mpsc::UnboundedSender<Utterance>, text: String, committed_at: Instant, first: &mut bool) {
        let _ = say.send(Utterance { text, committed_at, first_of_segment: *first });
        *first = false;
    }
    while let Some((segment, committed_at)) = segments.recv().await {
        let mut splitter = translate::SentenceSplitter::default();
        let mut first = true;
        // Each finished sentence goes to speech while Claude is still writing the next one.
        let result = tr
            .translate(&segment, |piece| {
                for sentence in splitter.push(piece) {
                    emit(&say, sentence, committed_at, &mut first);
                }
            })
            .await;
        match result {
            Ok(t) => {
                if let Some(rest) = splitter.finish() {
                    emit(&say, rest, committed_at, &mut first);
                }
                if t.text.is_empty() {
                    continue;
                }
                let fallback = if t.model != tr.model { format!(", відповіла {}", t.model) } else { String::new() };
                ui::line(&format!("{to} ◂ {}   [Claude {}{fallback}]", t.text, t.first_token.map(secs).unwrap_or_default()));
            }
            Err(e) => ui::line(&format!("переклад не вдався: {e:#}")),
        }
    }
}

async fn speak_loop(mut speaker: tts::Tts, mut said: mpsc::UnboundedReceiver<Utterance>, player: PlayerHandle) {
    while let Some(u) = said.recv().await {
        match speaker.speak(&u.text, &player).await {
            Ok(_) if u.first_of_segment => {
                ui::line(&format!("    ♪ звук пішов через {} після кінця фрази", secs(u.committed_at.elapsed())));
            }
            Ok(_) => {}
            Err(e) => ui::line(&format!("озвучення не вдалося: {e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_filler_only;
    use crate::translate::{MarkupFilter, SentenceSplitter};

    fn filter_all(pieces: &[&str]) -> String {
        let mut f = MarkupFilter::default();
        let mut out: String = pieces.iter().map(|p| f.push(p)).collect();
        out.push_str(&f.finish());
        out
    }

    #[test]
    fn drops_leaked_thinking_block() {
        let leaked = "<thinking>The segment is just a hesitation filler, but there is a word, \"Так\" — rendered as ええと in Japanese.</thinking>ええと、あー…";
        assert_eq!(filter_all(&[leaked]), "ええと、あー…");
        // Same text split at awkward points while streaming.
        let chars: Vec<char> = leaked.chars().collect();
        let pieces: Vec<String> = chars.chunks(3).map(|c| c.iter().collect()).collect();
        let refs: Vec<&str> = pieces.iter().map(String::as_str).collect();
        assert_eq!(filter_all(&refs), "ええと、あー…");
    }

    #[test]
    fn keeps_text_and_strips_bare_tags() {
        assert_eq!(filter_all(&["<segment>こんにちは</segment>"]), "こんにちは");
        assert_eq!(filter_all(&["今日は", "いい天気", "ですね。"]), "今日はいい天気ですね。");
        assert_eq!(filter_all(&["<context>Привіт</context>はい。"]), "はい。");
    }

    #[test]
    fn filler_detection() {
        assert!(is_filler_only("А-а-а."));
        assert!(is_filler_only("Ммм..."));
        assert!(is_filler_only("Ееее, ну"));
        assert!(!is_filler_only("Так, а-а-а..."));
        assert!(!is_filler_only("Ну, це дивно."));
    }

    #[test]
    fn splits_sentences_while_streaming() {
        let mut s = SentenceSplitter::default();
        let mut got = Vec::new();
        for p in ["こんにちは！今日", "は3.5度です。ま", "た"] {
            got.extend(s.push(p));
        }
        got.extend(s.finish());
        assert_eq!(got, vec!["こんにちは！", "今日は3.5度です。", "また"]);
    }
}
