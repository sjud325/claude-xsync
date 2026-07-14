use crate::mapper::PathMapper;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("no local mapping for token ${{{0}}} on this device")]
pub struct UnmappedToken(pub String);

pub fn key_to_portable(seg: &str, m: &PathMapper) -> String {
    for t in &m.tokens {
        // enc_local is ASCII by construction (encode_claude_path), so ASCII
        // case-insensitive prefix matching is exact — and unlike a
        // to_lowercase() comparison it cannot shift byte offsets on
        // multi-byte case-folding input (U+212A et al.) and panic on slicing.
        let enc = &t.enc_local;
        if seg.eq_ignore_ascii_case(enc) {
            return format!("${{{}}}", t.name);
        }
        if seg.len() > enc.len()
            && seg.is_char_boundary(enc.len())
            && seg[..enc.len()].eq_ignore_ascii_case(enc)
            && seg.as_bytes()[enc.len()] == b'-'
        {
            return format!("${{{}}}{}", t.name, &seg[enc.len()..]);
        }
    }
    seg.to_string()
}

pub fn portable_to_key(seg: &str, m: &PathMapper) -> Result<String, UnmappedToken> {
    if !seg.starts_with("${") {
        return Ok(seg.to_string());
    }
    for t in &m.tokens {
        let tok = format!("${{{}}}", t.name);
        if let Some(rest) = seg.strip_prefix(&tok) {
            return Ok(format!("{}{}", t.enc_local, rest));
        }
    }
    let name = seg[2..].split('}').next().unwrap_or("?").to_string();
    Err(UnmappedToken(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapper::PathMapper;
    use std::collections::BTreeMap;

    fn mac() -> PathMapper {
        PathMapper::new("/Users/woong", &BTreeMap::new()).unwrap()
    }
    fn win() -> PathMapper {
        PathMapper::new("C:\\Users\\Loki", &BTreeMap::new()).unwrap()
    }

    #[test]
    fn roundtrip_both_oses() {
        let p = key_to_portable("-Users-woong-workspace-foo", &mac());
        assert_eq!(p, "${HOME}-workspace-foo");
        assert_eq!(
            portable_to_key(&p, &win()).unwrap(),
            "C--Users-Loki-workspace-foo"
        );
        assert_eq!(
            portable_to_key(&p, &mac()).unwrap(),
            "-Users-woong-workspace-foo"
        );
    }

    #[test]
    fn boundary_blocks_woongho() {
        assert_eq!(
            key_to_portable("-Users-woongho-app", &mac()),
            "-Users-woongho-app"
        );
    }

    #[test]
    fn exact_home_key() {
        assert_eq!(key_to_portable("-Users-woong", &mac()), "${HOME}");
    }

    #[test]
    fn case_insensitive_key_match() {
        assert_eq!(key_to_portable("C--users-loki-ws", &win()), "${HOME}-ws");
    }

    #[test]
    fn multibyte_casefold_key_no_panic_no_match() {
        // U+212A (KELVIN SIGN) lowercases to ASCII 'k', shrinking the byte
        // length — must neither panic on a byte slice nor match.
        let seg = "C--Users-Lo\u{212A}i-ws";
        assert_eq!(key_to_portable(seg, &win()), seg);
    }

    #[test]
    fn unknown_token_is_error() {
        assert!(matches!(
            portable_to_key("${WORK}-x", &mac()),
            Err(UnmappedToken(_))
        ));
    }
}
