use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MapperError {
    #[error("token name {0:?} is reserved")]
    Reserved(String),
    #[error("invalid token name {0:?}: use [A-Z][A-Z0-9_]*")]
    BadName(String),
}

#[derive(Debug, Clone)]
pub struct TokenMapping {
    pub name: String,
    pub local: String,     // canonical case, no trailing separator
    pub enc_local: String, // Claude Code dir-encoding of `local`
}

#[derive(Debug, Clone)]
pub struct PathMapper {
    pub tokens: Vec<TokenMapping>,
}

pub fn encode_claude_path(p: &str) -> String {
    p.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn valid_name(n: &str) -> bool {
    let mut ch = n.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_uppercase())
        && ch.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl PathMapper {
    pub fn new(home: &str, path_map: &BTreeMap<String, String>) -> Result<Self, MapperError> {
        let mut tokens = Vec::new();
        let trim = |s: &str| s.trim_end_matches(['/', '\\']).to_string();
        for (local, name) in path_map {
            if name.eq_ignore_ascii_case("HOME") || name.eq_ignore_ascii_case("ESC") {
                return Err(MapperError::Reserved(name.clone()));
            }
            if !valid_name(name) {
                return Err(MapperError::BadName(name.clone()));
            }
            let local = trim(local);
            let enc_local = encode_claude_path(&local);
            tokens.push(TokenMapping {
                name: name.clone(),
                local,
                enc_local,
            });
        }
        let home = trim(home);
        let enc_local = encode_claude_path(&home);
        tokens.push(TokenMapping {
            name: "HOME".into(),
            local: home,
            enc_local,
        });
        tokens.sort_by_key(|t| std::cmp::Reverse(t.local.len()));
        Ok(PathMapper { tokens })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn home_always_mapped_and_longest_first() {
        let mut pm = BTreeMap::new();
        pm.insert("/Users/woong/work".into(), "WORK".into());
        let m = PathMapper::new("/Users/woong", &pm).unwrap();
        assert_eq!(m.tokens[0].name, "WORK"); // longer local first
        assert_eq!(m.tokens[1].name, "HOME");
        assert_eq!(m.tokens[1].local, "/Users/woong");
    }

    #[test]
    fn reserved_names_rejected() {
        for bad in ["HOME", "ESC", "home"] {
            let mut pm = BTreeMap::new();
            pm.insert("/x".into(), bad.into());
            assert!(PathMapper::new("/Users/woong", &pm).is_err());
        }
    }

    #[test]
    fn token_name_charset_enforced() {
        let mut pm = BTreeMap::new();
        pm.insert("/x".into(), "bad-name".into());
        assert!(PathMapper::new("/Users/woong", &pm).is_err());
    }

    #[test]
    fn windows_home_trailing_sep_trimmed_and_enc() {
        let m = PathMapper::new("C:\\Users\\Loki\\", &BTreeMap::new()).unwrap();
        assert_eq!(m.tokens[0].local, "C:\\Users\\Loki");
        assert_eq!(m.tokens[0].enc_local, "C--Users-Loki");
    }
}
