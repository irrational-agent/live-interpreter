//! Settings shared by the window and the engine, kept in %APPDATA%\live-interpreter\settings.json.

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

impl Settings {
    pub fn path() -> PathBuf {
        let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        base.join("live-interpreter").join("settings.json")
    }

    /// Saved settings, or the defaults when there are none or the file is unreadable.
    pub fn load() -> Settings {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
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
