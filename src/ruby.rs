//! Readings over the characters of a translation: furigana for Japanese, pinyin for Chinese.
//! A second Claude request marks the readings of the kanji or hanzi in
//! text that has already gone to the voice, so it never delays speech. The answer is accepted only
//! if, without its markup, it is exactly the translation; otherwise the text is shown without
//! readings rather than altered.

use crate::translate::{API_VERSION, FALLBACK_BETA, api_base_url};
use anyhow::{Result, bail};
use serde_json::{Value, json};

/// A stretch of text; `reading` is its furigana, and empty when it needs none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruby {
    pub text: String,
    pub reading: String,
}

pub fn plain_parts(text: &str) -> Vec<Ruby> {
    vec![Ruby { text: text.to_string(), reading: String::new() }]
}

pub fn is_kanji(c: char) -> bool {
    matches!(c, '\u{4E00}'..='\u{9FFF}' | '\u{3400}'..='\u{4DBF}' | '\u{F900}'..='\u{FAFF}' | '々')
}

pub fn has_kanji(text: &str) -> bool {
    text.chars().any(is_kanji)
}

fn flush_plain(parts: &mut Vec<Ruby>, plain: &mut String) {
    if !plain.is_empty() {
        parts.push(Ruby { text: std::mem::take(plain), reading: String::new() });
    }
}

/// Splits `{kanji|reading}` markup into parts. Anything that doesn't match the markup is kept as
/// plain text, so a literal brace can't lose characters.
pub fn parse_ruby(text: &str) -> Vec<Ruby> {
    let mut parts = Vec::new();
    let mut plain = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let marked = rest[open + 1..].find('}').and_then(|close| {
            let inner = &rest[open + 1..open + 1 + close];
            let (text, reading) = inner.split_once('|')?;
            (!text.is_empty()).then_some((text, reading, open + close + 2))
        });
        match marked {
            Some((text, reading, end)) => {
                plain.push_str(&rest[..open]);
                flush_plain(&mut parts, &mut plain);
                parts.push(Ruby { text: text.to_owned(), reading: reading.to_owned() });
                rest = &rest[end..];
            }
            None => {
                plain.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    plain.push_str(rest);
    flush_plain(&mut parts, &mut plain);
    parts
}

pub fn joined(parts: &[Ruby]) -> String {
    parts.iter().map(|p| p.text.as_str()).collect()
}

const JAPANESE: &str = "You add furigana to Japanese text. Copy the text exactly, character for character, and wrap every run of kanji in {kanji|reading}, where the reading is hiragana that fits this context. Keep okurigana and kana outside the braces, for example {食|た}べる, {日本語|にほんご}, {今日|きょう}は, {一人|ひとり}. Do not add, remove or change any other character, and do not translate. Reply with the marked text only. Do not include internal or system XML tags in your response.";

const CHINESE: &str = "You add pinyin to Chinese text. Copy the text exactly, character for character, and wrap every Chinese character in {character|pinyin}, one character per pair. Write the pinyin in lowercase with tone marks, as the character is pronounced in this sentence (tone changes of 一 and 不 included, neutral tone without a mark), for example {你|nǐ}{好|hǎo}, {不|bú}{是|shì}, {我|wǒ}{的|de}. Keep punctuation, digits and Latin letters outside the braces. Do not add, remove or change any other character, and do not translate. Reply with the marked text only. Do not include internal or system XML tags in your response.";

/// Languages that get readings over their characters: furigana for Japanese, pinyin for Chinese.
pub fn supports(language: &str) -> bool {
    matches!(language, "ja" | "zh")
}

#[derive(Clone)]
pub struct Furigana {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl Furigana {
    pub fn new(http: reqwest::Client, api_key: String, model: String) -> Self {
        Self { http, api_key, base_url: api_base_url(), model }
    }

    /// Marks the readings of `text` in `language`: "ja" gets furigana, "zh" gets pinyin.
    pub async fn annotate(&self, text: &str, language: &str) -> Result<Vec<Ruby>> {
        let system = if language == "zh" { CHINESE } else { JAPANESE };
        let body = json!({
            "model": self.model,
            "max_tokens": 16000,
            "output_config": { "effort": "low" },
            "fallbacks": "default",
            "system": system,
            "messages": [{ "role": "user", "content": text }],
        });
        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("anthropic-beta", FALLBACK_BETA)
            .json(&body)
            .send()
            .await?;
        let status = resp.status();
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("Claude API {status}: {body}");
        }
        let v: Value = serde_json::from_str(&body)?;
        if v["stop_reason"] == "refusal" {
            bail!("Claude відмовився розставити читання");
        }
        let answer: String = v["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect();
        let parts = parse_ruby(answer.trim());
        let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        if squash(&joined(&parts)) != squash(text) {
            bail!("розмітка читань змінила текст, тож її не показую");
        }
        Ok(parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markup_and_keeps_stray_braces() {
        let parts = parse_ruby("{今日|きょう}は{雨|あめ}です{笑}");
        assert_eq!(joined(&parts), "今日は雨です{笑}");
        assert_eq!(parts[0], Ruby { text: "今日".into(), reading: "きょう".into() });
        assert_eq!(parts[1].reading, "");
        assert_eq!(parts[2], Ruby { text: "雨".into(), reading: "あめ".into() });
    }

    #[test]
    fn detects_kanji() {
        assert!(has_kanji("駅はどこ"));
        assert!(!has_kanji("すみません、ありがとう"));
    }
}
