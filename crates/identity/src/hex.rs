//! Lower-case hex, because the key files and the fingerprints both need it.
//!
//! Hand-written rather than pulled in as a dependency: it is fifteen lines, and the
//! decoder here is stricter than most — it rejects odd lengths and non-hex bytes
//! instead of silently producing a shorter key, which is the failure mode that would
//! turn a corrupted identity file into a *different* valid identity.

pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(nibble(b >> 4));
        out.push(nibble(b & 0x0f));
    }
    out
}

fn nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        _ => (b'a' + n - 10) as char,
    }
}

/// Decode, or `None` if the input is not exactly `[0-9a-fA-F]` in even count.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = value(pair[0])?;
        let lo = value(pair[1])?;
        out.push(hi << 4 | lo);
    }
    Some(out)
}

fn value(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_byte_value() {
        let all: Vec<u8> = (0..=255u8).collect();
        assert_eq!(decode(&encode(&all)).unwrap(), all);
    }

    #[test]
    fn encodes_lower_case_and_decodes_either_case() {
        assert_eq!(encode(&[0xab, 0x0f]), "ab0f");
        assert_eq!(decode("AB0F").unwrap(), vec![0xab, 0x0f]);
    }

    #[test]
    fn an_odd_length_is_rejected_rather_than_truncated() {
        // Truncating would turn a damaged key into a shorter, still-parseable one.
        assert_eq!(decode("abc"), None);
    }

    #[test]
    fn a_non_hex_character_is_rejected() {
        assert_eq!(decode("zz"), None);
        assert_eq!(decode("ab cd"), None);
    }
}
