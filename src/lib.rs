//! live-interpreter: speak one language, hear another.
//! Microphone -> ElevenLabs Scribe realtime (speech-to-text) -> Claude (translation)
//! -> ElevenLabs text-to-speech (optionally in your cloned voice) -> headphones and a virtual mic,
//! with the translation also shown in the VRChat chatbox.

pub mod audio;
pub mod engine;
pub mod gui;
pub mod osc;
pub mod ruby;
pub mod settings;
pub mod stt;
pub mod translate;
pub mod tts;
pub mod tts_ws;
pub mod voices;
