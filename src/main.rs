//! Command-line front end. The window lives in `src/bin/live-interpreter-gui.rs`.

use anyhow::{Result, anyhow};
use clap::{Args, Parser, Subcommand};
use live_interpreter::audio::{self, Player};
use live_interpreter::engine::{self, Event};
use live_interpreter::settings::Settings;
use live_interpreter::{gui, translate, tts, voices};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    /// Зберегти іконку програми у файл .ico (для ярлика)
    Icon {
        /// Куди зберегти іконку
        out: PathBuf,
    },
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
    /// Модель озвучення: eleven_v4_turbo (якісна й швидка, добре передає клон),
    /// eleven_flash_v2_5 (найдешевша й найшвидша) або eleven_v4 (найякісніша, повільна)
    #[arg(long, default_value = "eleven_v4_turbo")]
    tts_model: String,
    /// Вихід звуку для прослуховування (частина назви)
    #[arg(long)]
    output_device: Option<String>,
    /// Ще один вихід, який іншим програмам служить мікрофоном, наприклад "Voicemeeter AUX Input"
    #[arg(long)]
    mic_output: Option<String>,
    /// Не грати переклад у вихід для прослуховування
    #[arg(long)]
    no_monitor: bool,
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
    /// Показувати переклад у чатбоксі VRChat через OSC
    #[arg(long)]
    vrchat: bool,
    /// Адреса OSC VRChat
    #[arg(long, default_value = "127.0.0.1:9000")]
    vrchat_address: String,
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

fn settings_from(lang: &LangArgs, voice: &VoiceArgs) -> Settings {
    Settings {
        from: lang.from.clone(),
        to: lang.to.clone(),
        model: lang.model.clone(),
        effort: lang.effort.clone(),
        voice: voice.voice.clone(),
        tts_model: voice.tts_model.clone(),
        monitor: !voice.no_monitor,
        monitor_device: voice.output_device.clone(),
        mic_output: voice.mic_output.clone(),
        ..Settings::default()
    }
}

/// Console output shared by all tasks: one live line for the partial transcript, and full
/// lines printed above it without tearing it apart.
mod console {
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

    pub fn line(text: &str) {
        let shown = PARTIAL.lock().unwrap();
        erase(&shown);
        println!("{text}");
        print!("{shown}");
        let _ = std::io::stdout().flush();
    }
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
        Cmd::Icon { out } => {
            gui::icon::write_ico(&out)?;
            println!("Іконку збережено в {}", out.display());
            Ok(())
        }
        Cmd::Record(a) => voices::record(&a.out, a.seconds, a.input_device.as_deref()).await,
        Cmd::Voices => {
            let keys = engine::load_keys()?;
            for v in voices::list(&engine::http_client()?, &keys.eleven).await? {
                println!("{:<12} {}  {}", v.category, v.id, v.name);
            }
            Ok(())
        }
        Cmd::Clone(a) => {
            let keys = engine::load_keys()?;
            println!("Завантажую {} файл(и) у ElevenLabs...", a.files.len());
            let http = engine::http_client()?;
            let (id, verify) =
                voices::clone(&http, &keys.eleven, &a.name, &a.files, a.remove_noise, a.description.as_deref()).await?;
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
    let keys = engine::load_keys()?;
    let http = engine::http_client()?;
    let settings = settings_from(&a.lang, &a.voice);
    let player = Player::start(&engine::output_choices(&settings, a.voice.mute))?;
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
    let mut speaker = tts::Speaker::new(http, keys.eleven, voice.id.clone(), a.voice.tts_model.clone(), lang, save.clone());
    let first = speaker.speak(&text, &player.handle()).await?;
    println!("голос «{}», {} · звук пішов через {}", voice.name, a.voice.tts_model, secs(first));
    player.handle().wait_idle().await;
    if let Some(s) = &save {
        tts::finish_wav(s)?;
    }
    Ok(())
}

async fn run(a: RunArgs) -> Result<()> {
    let keys = engine::load_keys()?;
    let mut settings = settings_from(&a.lang, &a.voice);
    settings.input_device = a.input_device.clone();
    settings.echo_guard = !a.headphones;
    settings.silence = a.silence;
    settings.turn_prediction = a.turn_prediction;
    settings.vrchat = a.vrchat;
    settings.vrchat_address = a.vrchat_address.clone();
    let (from, to, requested) = (settings.from.clone(), settings.to.clone(), settings.model.clone());
    let echo_note = settings.echo_guard && a.input_file.is_none() && !a.voice.mute;

    let emit: engine::Emit = Arc::new(move |event| match event {
        Event::Started { source, voice, outputs } => {
            console::line(&format!("Слухаю: {source}. {from} → {to}, голос «{voice}»"));
            let outputs = if outputs.is_empty() { "немає (звук вимкнено)".into() } else { outputs.join(", ") };
            console::line(&format!("Звук іде на: {outputs}"));
            if echo_note {
                console::line("Поки звучить переклад, мікрофон не слухаю, щоб не перекладати самого себе. У навушниках додай --headphones.");
            }
            console::line("Ctrl+C — вихід.\n");
        }
        Event::Partial(text) => console::partial(&text),
        Event::Segment { text, .. } => console::line(&format!("{from} ▸ {text}")),
        Event::Translated { text, claude, model, .. } => {
            let note = if model == requested || model.is_empty() { String::new() } else { format!(", відповіла {model}") };
            console::line(&format!("{to} ◂ {text}   [Claude {}{note}]", claude.map(secs).unwrap_or_default()));
        }
        Event::Furigana { .. } => {}
        Event::Spoken { after, .. } => console::line(&format!("    ♪ звук пішов через {} після кінця фрази", secs(after))),
        Event::Problem(text) => console::line(&text),
    });

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = stop_tx.send(true);
        }
    });
    let cfg = engine::Config { settings, input_file: a.input_file, save_output: a.voice.save_output, mute: a.voice.mute };
    engine::run(cfg, keys, emit, stop_rx).await
}
