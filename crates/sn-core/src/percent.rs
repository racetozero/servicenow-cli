//! Percent-encoding of URL parts.

use std::fmt::Write;

/// Encodes everything but the unreserved characters.
pub fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            true => out.push(byte as char),
            false => {
                let _ = write!(out, "%{byte:02X}");
            }
        }
    }
    out
}

/// Decodes `%XX`; a `+` stays a plus.
pub fn decode(text: &str) -> String {
    let value = text.replace('+', "%2B").replace('&', "%26");
    url::form_urlencoded::parse(format!("x={value}").as_bytes())
        .next()
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_survives_both_ways() {
        let text = "a&b=c+d 100%é^~";
        assert_eq!(encode(text), "a%26b%3Dc%2Bd%20100%25%C3%A9%5E~");
        assert_eq!(decode(&encode(text)), text);
        assert_eq!(decode("a&b+c%20d"), "a&b+c d");
    }
}
