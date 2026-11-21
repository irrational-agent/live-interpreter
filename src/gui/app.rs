//! The window: a transcript of what was said and how it was translated, with furigana over the
//! kanji, a start/stop button and a settings panel. The engine runs on its own thread and reports
//! back through a channel.

use crate::engine::{self, Event};
use crate::gui::{fonts, furigana, theme, widgets};
use crate::ruby::{self, Ruby};
use crate::settings::Settings;
use crate::voices;
use eframe::egui::{self, Align, FontFamily, FontId, Layout, Margin, RichText, Stroke, vec2};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

const MAX_ENTRIES: usize = 300;
const PROBLEM_SECONDS: f64 = 10.0;
const SETTINGS_WIDTH: f32 = 360.0;

const LANGUAGES: &[(&str, &str)] = &[
    ("uk", "українська"),
    ("ja", "японська"),
    ("en", "англійська"),
    ("de", "німецька"),
    ("fr", "французька"),
    ("es", "іспанська"),
    ("it", "італійська"),
    ("pl", "польська"),
    ("pt", "португальська"),
    ("cs", "чеська"),
    ("ko", "корейська"),
    ("zh", "китайська"),
    ("tr", "турецька"),
];

const TTS_MODELS: &[(&str, &str)] = &[
    ("eleven_v4_turbo", "Eleven v4 Turbo: якісно й швидко"),
    ("eleven_flash_v2_5", "Flash v2.5: найшвидша, клон звучить гірше"),
    ("eleven_v4", "Eleven v4: найякісніша, але повільна"),
];

const CLAUDE_MODELS: &[(&str, &str)] = &[
    ("claude-opus-5-5", "Claude Opus 5.5: найточніший переклад"),
    ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
    ("claude-haiku-5-5", "Claude Haiku 5.5: найшвидший"),
];

fn language_name(code: &str) -> &str {
    LANGUAGES.iter().find(|(c, _)| *c == code).map(|(_, n)| *n).unwrap_or(code)
}

fn seconds(d: Duration) -> String {
    format!("{:.1} с", d.as_secs_f32()).replace('.', ",")
}

enum Msg {
    Engine(Event),
    Stopped(Option<String>),
    Voices(Result<Vec<voices::Voice>, String>),
    ScreenshotSaved,
}

struct Entry {
    id: u64,
    original: String,
    translation: Option<String>,
    ruby: Option<Vec<Ruby>>,
    claude: Option<Duration>,
    spoken: Option<Duration>,
}

enum Status {
    Idle,
    Starting,
    Listening { source: String, outputs: Vec<String> },
}

struct Running {
    stop: tokio::sync::watch::Sender<bool>,
    settings: Settings,
}

/// Lets the app check itself: interpret a file without sound, then save a screenshot and quit.
/// Driven by LIVE_INTERPRETER_TEST_INPUT, LIVE_INTERPRETER_SCREENSHOT and
/// LIVE_INTERPRETER_SHOW_SETTINGS.
struct SelfTest {
    input: Option<PathBuf>,
    screenshot: PathBuf,
    started: bool,
    finished_at: Option<f64>,
    requested: bool,
}

pub struct App {
    settings: Settings,
    persist: bool,
    running: Option<Running>,
    status: Status,
    error: Option<String>,
    problems: Vec<(String, f64)>,
    entries: Vec<Entry>,
    partial: String,
    show_settings: bool,
    voices: Option<Result<Vec<voices::Voice>, String>>,
    inputs: (Vec<String>, Option<String>),
    outputs: (Vec<String>, Option<String>),
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    ctx: egui::Context,
    selftest: Option<SelfTest>,
    /// Stop was pressed by "restart": start again once the engine has stopped.
    restart_pending: bool,
}

fn window_handle(cc: &eframe::CreationContext<'_>) -> isize {
    match cc.window_handle().map(|handle| handle.as_raw()) {
        Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
        _ => 0,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        fonts::install(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx);
        theme::style_title_bar(window_handle(cc));

        let selftest = std::env::var_os("LIVE_INTERPRETER_SCREENSHOT").map(|shot| SelfTest {
            input: std::env::var_os("LIVE_INTERPRETER_TEST_INPUT").map(PathBuf::from),
            screenshot: PathBuf::from(shot),
            started: false,
            finished_at: None,
            requested: false,
        });
        let (tx, rx) = mpsc::channel();
        let mut app = Self {
            settings: Settings::load(),
            persist: selftest.is_none(),
            running: None,
            status: Status::Idle,
            error: None,
            problems: Vec::new(),
            entries: Vec::new(),
            partial: String::new(),
            show_settings: std::env::var_os("LIVE_INTERPRETER_SHOW_SETTINGS").is_some(),
            voices: None,
            inputs: crate::audio::input_names(),
            outputs: crate::audio::output_names(),
            tx,
            rx,
            ctx: cc.egui_ctx.clone(),
            selftest,
            restart_pending: false,
        };
        app.load_voices();
        app
    }

    fn load_voices(&mut self) {
        let keys = match engine::load_keys() {
            Ok(keys) => keys,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| {
                    rt.block_on(async {
                        let http = engine::http_client().map_err(|e| e.to_string())?;
                        voices::list(&http, &keys.eleven).await.map_err(|e| format!("{e:#}"))
                    })
                });
            let _ = tx.send(Msg::Voices(result));
            ctx.request_repaint();
        });
    }

    fn start(&mut self) {
        let keys = match engine::load_keys() {
            Ok(keys) => keys,
            Err(e) => {
                self.error = Some(format!("{e:#}"));
                return;
            }
        };
        let test_input = self.selftest.as_ref().and_then(|t| t.input.clone());
        let cfg = engine::Config {
            settings: self.settings.clone(),
            mute: test_input.is_some(),
            input_file: test_input,
            save_output: None,
        };
        let (stop, stop_rx) = tokio::sync::watch::channel(false);
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        let (emit_tx, emit_ctx) = (tx.clone(), ctx.clone());
        let emit: engine::Emit = Arc::new(move |event| {
            let _ = emit_tx.send(Msg::Engine(event));
            emit_ctx.request_repaint();
        });
        // cpal streams must stay on the thread that opened them, so the engine gets its own.
        std::thread::spawn(move || {
            let result = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
                Ok(rt) => rt.block_on(engine::run(cfg, keys, emit, stop_rx)),
                Err(e) => Err(e.into()),
            };
            let _ = tx.send(Msg::Stopped(result.err().map(|e| format!("{e:#}"))));
            ctx.request_repaint();
        });
        self.running = Some(Running { stop, settings: self.settings.clone() });
        self.status = Status::Starting;
        self.error = None;
        self.partial.clear();
    }

    fn stop(&mut self) {
        if let Some(running) = &self.running {
            let _ = running.stop.send(true);
        }
    }

    fn entry(&mut self, id: u64) -> Option<&mut Entry> {
        self.entries.iter_mut().rev().find(|e| e.id == id)
    }

    fn receive(&mut self, now: f64) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Engine(Event::Started { source, outputs, .. }) => self.status = Status::Listening { source, outputs },
                Msg::Engine(Event::Partial(text)) => self.partial = text,
                Msg::Engine(Event::Segment { id, text }) => {
                    self.partial.clear();
                    self.entries.push(Entry { id, original: text, translation: None, ruby: None, claude: None, spoken: None });
                    if self.entries.len() > MAX_ENTRIES {
                        self.entries.remove(0);
                    }
                }
                Msg::Engine(Event::Translated { id, text, claude, .. }) => {
                    if let Some(entry) = self.entry(id) {
                        entry.translation = Some(text);
                        entry.claude = claude;
                    }
                }
                Msg::Engine(Event::Furigana { id, parts }) => {
                    if let Some(entry) = self.entry(id) {
                        entry.ruby = Some(parts);
                    }
                }
                Msg::Engine(Event::Spoken { id, after }) => {
                    if let Some(entry) = self.entry(id) {
                        entry.spoken = Some(after);
                    }
                }
                Msg::Engine(Event::Problem(text)) => self.problems.push((text, now)),
                Msg::Stopped(error) => {
                    self.running = None;
                    self.status = Status::Idle;
                    self.partial.clear();
                    if error.is_some() {
                        self.error = error;
                    }
                    if let Some(test) = &mut self.selftest {
                        test.finished_at = Some(now);
                    }
                }
                Msg::Voices(result) => self.voices = Some(result),
                Msg::ScreenshotSaved => self.ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
        self.problems.retain(|(_, at)| now - at < PROBLEM_SECONDS);
    }

    fn drive_selftest(&mut self, now: f64) {
        let Some(test) = &mut self.selftest else { return };
        if !test.started {
            test.started = true;
            if test.input.is_some() {
                self.start();
            } else {
                test.finished_at = Some(now);
            }
            return;
        }
        // A few seconds after the run, so furigana has arrived; or give up after two minutes.
        let shot_at = std::env::var("LIVE_INTERPRETER_SHOT_AT").ok().and_then(|v| v.parse::<f64>().ok());
        let ready = match shot_at {
            Some(at) => now > at,
            None => test.finished_at.is_some_and(|t| now - t > 4.0) || now > 120.0,
        };
        if ready && !test.requested {
            test.requested = true;
            let (path, tx, ctx) = (test.screenshot.clone(), self.tx.clone(), self.ctx.clone());
            self.ctx.request_screenshot(move |image| {
                let [w, h] = image.size;
                let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                if let Err(e) = image::save_buffer(&path, &rgba, w as u32, h as u32, image::ColorType::Rgba8) {
                    eprintln!("знімок не збережено: {e}");
                }
                let _ = tx.send(Msg::ScreenshotSaved);
                ctx.request_repaint();
            });
        }
        self.ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn voice_label(&self) -> String {
        let name = &self.settings.voice;
        name.split(" - ").next().unwrap_or(name).trim().to_string()
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (mark, _) = ui.allocate_exact_size(vec2(38.0, 38.0), egui::Sense::hover());
            ui.painter().rect_filled(mark, 10, theme::ACCENT);
            ui.painter().text(
                mark.center(),
                egui::Align2::CENTER_CENTER,
                "通",
                FontId::new(22.0, FontFamily::Name(fonts::JAPANESE_DISPLAY.into())),
                theme::ACCENT_TEXT,
            );
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.add_space(1.0);
                ui.label(RichText::new("Синхронний перекладач").font(theme::semibold(17.0)).color(theme::TEXT));
                let s = &self.settings;
                ui.label(
                    RichText::new(format!(
                        "{} → {} · голос «{}»",
                        language_name(&s.from),
                        language_name(&s.to),
                        self.voice_label()
                    ))
                    .size(13.0)
                    .color(theme::TEXT_MUTED),
                );
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (icon, hint) = if self.show_settings {
                    (widgets::Icon::Close, "Сховати налаштування")
                } else {
                    (widgets::Icon::Gear, "Показати налаштування")
                };
                if widgets::icon_button(ui, icon, hint).clicked() {
                    self.show_settings = !self.show_settings;
                }
                ui.add_space(4.0);
                match self.status {
                    Status::Idle => {
                        if widgets::primary_button(ui, "Почати слухати", true, true).clicked() {
                            self.start();
                        }
                    }
                    Status::Starting => {
                        widgets::primary_button(ui, "Під'єднуюся…", false, false);
                    }
                    Status::Listening { .. } => {
                        if widgets::primary_button(ui, "Зупинити", false, true).clicked() {
                            self.stop();
                        }
                    }
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let (color, pulsing, text) = match &self.status {
                Status::Idle => (theme::TEXT_FAINT, false, "Зупинено".to_string()),
                Status::Starting => (theme::RUBY, true, "Під'єднуюся до ElevenLabs…".to_string()),
                Status::Listening { source, outputs } => {
                    let mut text = format!("Слухаю «{source}»");
                    if !outputs.is_empty() {
                        text.push_str(&format!(" · звук: {}", outputs.join(", ")));
                    }
                    if self.running.as_ref().is_some_and(|r| r.settings.vrchat) {
                        text.push_str(" · чатбокс VRChat");
                    }
                    (theme::OK, true, text)
                }
            };
            widgets::status_dot(ui, color, pulsing);
            ui.label(RichText::new(text).size(12.5).color(theme::TEXT_FAINT));
        });
        if let Some(error) = &self.error {
            ui.add_space(6.0);
            widgets::error_banner(ui, error);
        }
        for (problem, _) in &self.problems {
            ui.label(RichText::new(problem).size(12.5).color(theme::ERROR));
        }
    }

    fn transcript(&mut self, ui: &mut egui::Ui) {
        if self.entries.is_empty() && self.partial.is_empty() {
            let (title, body) = match self.status {
                Status::Listening { .. } => (
                    "Слухаю".to_string(),
                    format!("Говори, і тут з'являться твої фрази та їхній переклад ({}).", language_name(&self.settings.to)),
                ),
                _ => ("Тут буде розмова".to_string(), "Натисни «Почати слухати» і говори.".to_string()),
            };
            widgets::centered_note(ui, &title, &body);
            return;
        }
        let japanese = self.running.as_ref().map_or(&self.settings, |r| &r.settings).to == "ja";
        let style = furigana::TextStyle {
            base: FontId::new(25.0, FontFamily::Name(fonts::JAPANESE_DISPLAY.into())),
            ruby: FontId::proportional(12.0),
            base_color: theme::TEXT,
            ruby_color: theme::RUBY,
            row_gap: 4.0,
        };
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(4.0);
            for entry in &self.entries {
                theme::card_frame().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(RichText::new(&entry.original).size(14.5).color(theme::TEXT_MUTED));
                    ui.add_space(4.0);
                    match &entry.translation {
                        Some(text) if japanese => {
                            let parts = entry.ruby.clone().unwrap_or_else(|| ruby::plain_parts(text));
                            furigana::show(ui, &parts, &style);
                        }
                        Some(text) => {
                            ui.label(RichText::new(text).size(22.0).color(theme::TEXT));
                        }
                        None => widgets::skeleton(ui, &[(0.55, 24.0)]),
                    }
                    let mut meta = Vec::new();
                    if let Some(c) = entry.claude {
                        meta.push(format!("переклад {}", seconds(c)));
                    }
                    if let Some(s) = entry.spoken {
                        meta.push(format!("звук через {}", seconds(s)));
                    }
                    if !meta.is_empty() {
                        ui.add_space(2.0);
                        ui.label(RichText::new(meta.join(" · ")).size(11.5).color(theme::TEXT_FAINT));
                    }
                });
                ui.add_space(10.0);
            }
            if !self.partial.is_empty() {
                egui::Frame::new()
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .corner_radius(theme::RADIUS_CARD)
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            widgets::status_dot(ui, theme::RUBY, true);
                            ui.label(RichText::new(&self.partial).size(14.5).italics().color(theme::TEXT_MUTED));
                        });
                    });
                ui.add_space(10.0);
            }
        });
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 6.0;
            let width = ui.available_width();

            widgets::section_label(ui, "МОВИ");
            let langs: Vec<(String, String)> = LANGUAGES.iter().map(|(c, n)| (c.to_string(), n.to_string())).collect();
            combo(ui, "Я говорю", width, &mut self.settings.from, &langs);
            combo(ui, "Перекладати на", width, &mut self.settings.to, &langs);

            ui.add_space(14.0);
            widgets::section_label(ui, "ГОЛОС");
            let voice_options = self.voice_options();
            combo(ui, "Озвучувати голосом", width, &mut self.settings.voice, &voice_options);
            if let Some(Err(e)) = &self.voices {
                widgets::hint(ui, &format!("Не вдалося отримати голоси: {e}"));
            }
            let tts: Vec<(String, String)> = TTS_MODELS.iter().map(|(c, n)| (c.to_string(), n.to_string())).collect();
            combo(ui, "Модель озвучення", width, &mut self.settings.tts_model, &tts);

            ui.add_space(14.0);
            widgets::section_label(ui, "ЗВУК");
            let mic_default = format!("Типовий: {}", self.inputs.1.clone().unwrap_or_default());
            let mics = device_options(&self.inputs.0, &mic_default);
            combo(ui, "Слухати мікрофон", width, &mut self.settings.input_device, &mics);
            ui.add_space(4.0);
            widgets::toggle(ui, &mut self.settings.monitor, "Грати переклад у навушники");
            if self.settings.monitor {
                let out_default = format!("Типовий: {}", self.outputs.1.clone().unwrap_or_default());
                let outs = device_options(&self.outputs.0, &out_default);
                combo(ui, "Навушники", width, &mut self.settings.monitor_device, &outs);
            }
            ui.add_space(4.0);
            let mic_outs = device_options(&self.outputs.0, "Вимкнено");
            combo(ui, "Подавати переклад як мікрофон", width, &mut self.settings.mic_output, &mic_outs);
            widgets::hint(
                ui,
                "Для VRChat вибери Voicemeeter AUX Input, а у Voicemeeter направ AUX на шину, яку VRChat слухає як мікрофон.",
            );
            ui.add_space(4.0);
            widgets::toggle(ui, &mut self.settings.echo_guard, "Не слухати мікрофон, поки звучить переклад");
            widgets::hint(
                ui,
                "Увімкни, якщо мікрофон чує переклад: з колонок або через ту саму шину Voicemeeter. Інакше програма перекладатиме сама себе.",
            );
            if let Some(warning) = feedback_warning(&self.settings, self.inputs.1.as_deref()) {
                ui.add_space(2.0);
                ui.label(RichText::new(warning).size(12.5).color(theme::RUBY));
            }

            ui.add_space(14.0);
            widgets::section_label(ui, "VRCHAT");
            widgets::toggle(ui, &mut self.settings.vrchat, "Показувати переклад у чатбоксі");
            if self.settings.vrchat {
                widgets::toggle(ui, &mut self.settings.vrchat_original, "Додавати мою фразу над перекладом");
                ui.label(RichText::new("Адреса OSC").size(12.5).color(theme::TEXT_MUTED));
                ui.add(egui::TextEdit::singleline(&mut self.settings.vrchat_address).desired_width(width));
                widgets::hint(ui, "У VRChat має бути ввімкнено OSC: Action Menu → Options → OSC → Enabled.");
            }

            ui.add_space(14.0);
            widgets::section_label(ui, "ПЕРЕКЛАД");
            let models: Vec<(String, String)> = CLAUDE_MODELS.iter().map(|(c, n)| (c.to_string(), n.to_string())).collect();
            combo(ui, "Модель Claude", width, &mut self.settings.model, &models);
            ui.add_space(4.0);
            if self.settings.to == "ja" {
                widgets::toggle(ui, &mut self.settings.furigana, "Показувати фуріґану над кандзі");
            }
            ui.label(RichText::new("Пауза, після якої фраза закінчена").size(12.5).color(theme::TEXT_MUTED));
            ui.add(egui::Slider::new(&mut self.settings.silence, 0.3..=2.0).step_by(0.1).suffix(" с"));
            ui.add_space(16.0);
        });

        if self.settings != before && self.persist {
            if let Err(e) = self.settings.save() {
                self.error = Some(format!("налаштування не збереглися: {e:#}"));
            }
        }
    }

    fn voice_options(&self) -> Vec<(String, String)> {
        let rank = |category: &str| match category {
            "cloned" => 0,
            "professional" => 1,
            "generated" => 2,
            _ => 3,
        };
        let mut options: Vec<(String, String)> = match &self.voices {
            Some(Ok(list)) => {
                let mut list: Vec<&voices::Voice> = list.iter().collect();
                list.sort_by_key(|v| rank(&v.category));
                list.iter()
                    .map(|v| {
                        let tag = match v.category.as_str() {
                            "cloned" => " · клон",
                            "professional" => " · бібліотека",
                            _ => "",
                        };
                        (v.name.clone(), format!("{}{tag}", v.name))
                    })
                    .collect()
            }
            _ => Vec::new(),
        };
        if !options.iter().any(|(value, _)| *value == self.settings.voice) {
            options.insert(0, (self.settings.voice.clone(), self.settings.voice.clone()));
        }
        options
    }

    fn restart_note(&mut self, ui: &mut egui::Ui) {
        let changed = self.running.as_ref().is_some_and(|r| r.settings != self.settings);
        if changed && matches!(self.status, Status::Listening { .. }) {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Зміни запрацюють після перезапуску.").size(12.5).color(theme::RUBY));
                if ui.button("Перезапустити").clicked() {
                    self.stop();
                    self.restart_pending = true;
                }
            });
        }
    }
}

/// Warns when the translation goes into a Voicemeeter input while the interpreter listens to a
/// Voicemeeter bus: on this machine AUX Input reaches bus B1, so the interpreter would hear its
/// own speech and translate it again.
fn feedback_warning(s: &Settings, default_input: Option<&str>) -> Option<&'static str> {
    let mic_output = s.mic_output.as_deref()?.to_lowercase();
    let input = s.input_device.as_deref().or(default_input).unwrap_or("").to_lowercase();
    let input_is_bus = input.contains("voicemeeter out") || input.contains("voicemeeter output");
    (mic_output.contains("voicemeeter") && input_is_bus && !s.echo_guard).then_some(
        "Переклад іде у Voicemeeter, а перекладач слухає шину Voicemeeter. Якщо AUX потрапляє в цю шину,          програма почує себе й перекладатиме свій переклад. Вибери вище фізичний мікрофон або ввімкни          «Не слухати мікрофон, поки звучить переклад».",
    )
}

/// A labelled drop-down filling `width`. Returns true when the choice changed.
fn combo<T: PartialEq + Clone>(ui: &mut egui::Ui, label: &str, width: f32, value: &mut T, options: &[(T, String)]) -> bool {
    ui.label(RichText::new(label).size(12.5).color(theme::TEXT_MUTED));
    let selected = options.iter().find(|(v, _)| v == value).map(|(_, l)| l.clone()).unwrap_or_default();
    let mut changed = false;
    egui::ComboBox::from_id_salt(label).selected_text(selected).width(width).show_ui(ui, |ui| {
        for (option, text) in options {
            if ui.selectable_label(option == value, text).clicked() && option != value {
                *value = option.clone();
                changed = true;
            }
        }
    });
    changed
}

/// "Default"/"off" first, then every device by its full name.
fn device_options(names: &[String], none_label: &str) -> Vec<(Option<String>, String)> {
    std::iter::once((None, none_label.to_string()))
        .chain(names.iter().map(|n| (Some(n.clone()), n.clone())))
        .collect()
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        self.receive(now);
        self.drive_selftest(now);
        if self.restart_pending && self.running.is_none() {
            self.restart_pending = false;
            self.start();
        }

        egui::Panel::top("header")
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin { left: 20, right: 20, top: 16, bottom: 10 }))
            .show_separator_line(false)
            .show(ui, |ui| {
                self.header(ui);
                self.restart_note(ui);
            });
        if self.show_settings {
            egui::Panel::right("settings")
                .exact_size(SETTINGS_WIDTH)
                .resizable(false)
                .show_separator_line(false)
                .frame(egui::Frame::new().fill(theme::CARD).inner_margin(Margin::same(18)))
                .show(ui, |ui| self.settings_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(Margin { left: 20, right: 20, top: 4, bottom: 12 }))
            .show(ui, |ui| self.transcript(ui));
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.stop();
    }
}
