//! Settings shared by the window and the engine, kept in `settings.json` in the project folder.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Language the speaker uses (ISO 639-1).
    pub from: String,
    /// Language of the translation (ISO 639-1).
    pub to: String,
    /// Claude model for translation and furigana.
    pub model: String,
    pub effort: String,
    /// Voice name on the ElevenLabs account, or a voice ID.
    pub voice: String,
    pub tts_model: String,
    /// Microphone; `None` is the system default.
    pub input_device: Option<String>,
    /// Play the translation to the listening output (headphones).
    pub monitor: bool,
    /// Listening output; `None` is the system default.
    pub monitor_device: Option<String>,
    /// Second output that acts as a microphone for other apps, e.g. Voicemeeter AUX Input.
    pub mic_output: Option<String>,
    /// Ignore the microphone while the translation plays, so speakers can't feed it back.
    pub echo_guard: bool,
    pub silence: f32,
    pub turn_prediction: bool,
    /// Show the translation in the VRChat chatbox over OSC.
    pub vrchat: bool,
    pub vrchat_address: String,
    /// Put the original phrase above the translation in the chatbox.
    pub vrchat_original: bool,
    /// Furigana above the kanji of Japanese translations.
    pub furigana: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            from: "uk".into(),
            to: "ja".into(),
            model: "claude-opus-5-5".into(),
            effort: "low".into(),
            voice: "George".into(),
            tts_model: "eleven_v4_turbo".into(),
            input_device: None,
            monitor: true,
            monitor_device: None,
            mic_output: None,
            echo_guard: false,
            silence: 0.7,
            turn_prediction: false,
            vrchat: false,
            vrchat_address: "127.0.0.1:9000".into(),
            vrchat_original: false,
            furigana: true,
        }
    }
}

/// The project folder: the working directory or the nearest folder above the executable that holds
/// `.env.local`. Keys and settings both live there.
pub fn project_dir() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::current_dir().into_iter().collect();
    if let Ok(exe) = std::env::current_exe() {
        dirs.extend(exe.ancestors().skip(1).map(PathBuf::from));
    }
    dirs.into_iter().find(|d| d.join(".env.local").is_file())
}

impl Settings {
    /// `settings.json` in the project folder, next to `.env.local`.
    ///
    /// Not in %APPDATA%: when the Claude desktop app (an MSIX package) starts a program, Windows
    /// redirects that program's AppData writes into the package, so a window opened from the
    /// desktop shortcut would see different settings than one started from Claude.
    /// LIVE_INTERPRETER_SETTINGS points the self-test at a file of its own.
    pub fn path() -> PathBuf {
        if let Some(path) = std::env::var_os("LIVE_INTERPRETER_SETTINGS") {
            return PathBuf::from(path);
        }
        let dir = project_dir()
            .or_else(|| std::env::current_exe().ok().and_then(|exe| exe.parent().map(PathBuf::from)))
            .unwrap_or_else(|| PathBuf::from("."));
        dir.join("settings.json")
    }

    /// Where earlier versions kept the settings; read once if the new file does not exist yet.
    fn legacy_path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|base| PathBuf::from(base).join("live-interpreter").join("settings.json"))
    }

    /// Saved settings, or the defaults when there are none or the file is unreadable.
    pub fn load() -> Settings {
        let read = |path: PathBuf| -> Option<Settings> {
            serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
        };
        let current = Self::path();
        if current.is_file() || std::env::var_os("LIVE_INTERPRETER_SETTINGS").is_some() {
            return read(current).unwrap_or_default();
        }
        Self::legacy_path().and_then(read).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let s: Settings = serde_json::from_str(r#"{"to":"en","vrchat":true}"#).unwrap();
        assert_eq!(s.to, "en");
        assert!(s.vrchat);
        assert_eq!(s.from, "uk");
        assert_eq!(s.vrchat_address, "127.0.0.1:9000");
    }
}
