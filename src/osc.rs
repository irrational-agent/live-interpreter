//! VRChat chatbox over OSC: UDP datagrams to `/chatbox/input` on VRChat's OSC port.

use anyhow::{Context, Result, anyhow};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

/// VRChat shows at most this many characters in the chatbox.
pub const MAX_CHARS: usize = 144;

enum Arg<'a> {
    Str(&'a str),
    Bool(bool),
}

fn pad(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

fn push_string(buf: &mut Vec<u8>, s: &str) {
    buf.extend_from_slice(s.as_bytes());
    buf.push(0);
    pad(buf);
}

/// Encodes one OSC message. Booleans carry no payload: they live in the type tags as T or F.
fn encode(address: &str, args: &[Arg]) -> Vec<u8> {
    let mut buf = Vec::new();
    push_string(&mut buf, address);
    let tags: String = std::iter::once(',')
        .chain(args.iter().map(|a| match a {
            Arg::Str(_) => 's',
            Arg::Bool(true) => 'T',
            Arg::Bool(false) => 'F',
        }))
        .collect();
    push_string(&mut buf, &tags);
    for arg in args {
        if let Arg::Str(s) = arg {
            push_string(&mut buf, s);
        }
    }
    buf
}

/// Fits text into the chatbox, cutting the end with an ellipsis when it is too long.
pub fn fit(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= MAX_CHARS {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(MAX_CHARS - 1).collect();
    cut.push('…');
    cut
}

pub struct Chatbox {
    socket: UdpSocket,
    target: SocketAddr,
}

impl Chatbox {
    pub fn new(address: &str) -> Result<Self> {
        let target = address
            .to_socket_addrs()
            .with_context(|| format!("неправильна адреса OSC «{address}»"))?
            .next()
            .ok_or_else(|| anyhow!("неправильна адреса OSC «{address}»"))?;
        let socket = UdpSocket::bind(if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
        Ok(Self { socket, target })
    }

    /// Shows `text` in the chatbox at once, without the notification sound.
    pub fn send(&self, text: &str) -> Result<()> {
        let text = fit(text);
        let packet = encode("/chatbox/input", &[Arg::Str(&text), Arg::Bool(true), Arg::Bool(false)]);
        self.socket.send_to(&packet, self.target)?;
        Ok(())
    }

    /// Turns the "typing" bubble above the avatar on or off.
    pub fn typing(&self, on: bool) -> Result<()> {
        self.socket.send_to(&encode("/chatbox/typing", &[Arg::Bool(on)]), self.target)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_chatbox_message_with_padding() {
        let packet = encode("/chatbox/input", &[Arg::Str("hi"), Arg::Bool(true), Arg::Bool(false)]);
        let mut expected = b"/chatbox/input\0\0".to_vec();
        expected.extend_from_slice(b",sTF\0\0\0\0");
        expected.extend_from_slice(b"hi\0\0");
        assert_eq!(packet, expected);
        assert_eq!(packet.len() % 4, 0);
    }

    #[test]
    fn utf8_text_is_padded_by_bytes() {
        let packet = encode("/a", &[Arg::Str("駅")]);
        // "/a\0\0" + ",s\0\0" + 3 bytes of 駅 + NUL = 4
        assert_eq!(packet.len(), 12);
    }

    #[test]
    fn long_text_is_cut_to_the_chatbox_limit() {
        let long = "あ".repeat(200);
        let fitted = fit(&long);
        assert_eq!(fitted.chars().count(), MAX_CHARS);
        assert!(fitted.ends_with('…'));
        assert_eq!(fit("  short "), "short");
    }
}
