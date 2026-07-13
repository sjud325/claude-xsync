use crate::mapper::PathMapper;
use thiserror::Error;

#[derive(Debug, Error)]
#[error("no local mapping for token ${{{0}}} on this device")]
pub struct UnmappedToken(pub String);

pub fn key_to_portable(seg: &str, m: &PathMapper) -> String {
    for t in &m.tokens {
        let enc = &t.enc_local;
        let seg_l = seg.to_lowercase();
        let enc_l = enc.to_lowercase();
        if seg_l == enc_l {
            return format!("${{{}}}", t.name);
        }
        if seg_l.starts_with(&format!("{enc_l}-")) {
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
    fn unknown_token_is_error() {
        assert!(matches!(
            portable_to_key("${WORK}-x", &mac()),
            Err(UnmappedToken(_))
        ));
    }
}
