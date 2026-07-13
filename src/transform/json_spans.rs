use thiserror::Error;

#[derive(Debug, Error)]
pub enum LexError {
    #[error("truncated or malformed JSON at byte {0}")]
    Malformed(usize),
    #[error("invalid escape at byte {0}")]
    BadEscape(usize),
    #[error("invalid utf8/surrogate at byte {0}")]
    BadUnicode(usize),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrSpan {
    pub start: usize,
    pub end: usize,
}

/// Scan a single JSON document (one JSONL line) and return byte ranges of the
/// escaped CONTENT of every string token (keys and values), quotes excluded.
/// Non-string bytes are only validated enough to find string boundaries.
pub fn string_spans(line: &[u8]) -> Result<Vec<StrSpan>, LexError> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < line.len() {
        match line[i] {
            b'"' => {
                let start = i + 1;
                i += 1;
                loop {
                    if i >= line.len() {
                        return Err(LexError::Malformed(i));
                    }
                    match line[i] {
                        b'\\' => {
                            if i + 1 >= line.len() {
                                return Err(LexError::Malformed(i));
                            }
                            i += 2;
                        }
                        b'"' => {
                            spans.push(StrSpan { start, end: i });
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            _ => i += 1,
        }
    }
    if spans.is_empty() && line.contains(&b'"') {
        return Err(LexError::Malformed(0));
    }
    Ok(spans)
}

pub fn decode_json_string(raw: &[u8]) -> Result<String, LexError> {
    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'\\' {
            i += 1;
            if i >= raw.len() {
                return Err(LexError::BadEscape(i));
            }
            match raw[i] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let hex = |j: usize| -> Result<u32, LexError> {
                        if j + 4 > raw.len() {
                            return Err(LexError::BadUnicode(j));
                        }
                        u32::from_str_radix(
                            std::str::from_utf8(&raw[j..j + 4])
                                .map_err(|_| LexError::BadUnicode(j))?,
                            16,
                        )
                        .map_err(|_| LexError::BadUnicode(j))
                    };
                    let mut cp = hex(i + 1)?;
                    i += 4;
                    if (0xD800..0xDC00).contains(&cp) {
                        if raw.get(i + 1) == Some(&b'\\') && raw.get(i + 2) == Some(&b'u') {
                            let lo = hex(i + 3)?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return Err(LexError::BadUnicode(i));
                            }
                            cp = 0x10000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                            i += 6;
                        } else {
                            return Err(LexError::BadUnicode(i));
                        }
                    } else if (0xDC00..0xE000).contains(&cp) {
                        return Err(LexError::BadUnicode(i));
                    }
                    out.push(char::from_u32(cp).ok_or(LexError::BadUnicode(i))?);
                }
                _ => return Err(LexError::BadEscape(i)),
            }
            i += 1;
        } else {
            let s = std::str::from_utf8(&raw[i..]).map_err(|_| LexError::BadUnicode(i))?;
            let c = s.chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    Ok(out)
}

pub fn encode_json_string(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.extend(b"\\\""),
            '\\' => out.extend(b"\\\\"),
            '\n' => out.extend(b"\\n"),
            '\r' => out.extend(b"\\r"),
            '\t' => out.extend(b"\\t"),
            c if (c as u32) < 0x20 => out.extend(format!("\\u{:04x}", c as u32).into_bytes()),
            c => {
                let mut b = [0u8; 4];
                out.extend(c.encode_utf8(&mut b).as_bytes());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_keys_and_values() {
        let line = br#"{"cwd":"/Users/woong","n":3,"ok":true}"#;
        let spans = string_spans(line).unwrap();
        let texts: Vec<String> = spans
            .iter()
            .map(|s| decode_json_string(&line[s.start..s.end]).unwrap())
            .collect();
        assert_eq!(texts, vec!["cwd", "/Users/woong", "n", "ok"]);
    }

    #[test]
    fn handles_escapes_and_unicode() {
        let line = r#"{"p":"C:\\Users\\Loki\\한글"}"#.as_bytes();
        let spans = string_spans(line).unwrap();
        let v = decode_json_string(&line[spans[1].start..spans[1].end]).unwrap();
        assert_eq!(v, "C:\\Users\\Loki\\한글");
    }

    #[test]
    fn rejects_truncated_line() {
        assert!(string_spans(br#"{"cwd":"/Users/wo"#).is_err());
    }

    #[test]
    fn encode_roundtrip() {
        let s = "C:\\x \"q\" 한글\n";
        let enc = encode_json_string(s);
        assert_eq!(decode_json_string(&enc).unwrap(), s);
    }
}

#[cfg(test)]
mod prop {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        // Oracle: on any serde-accepted JSON line, our spans decode to exactly
        // the strings serde sees (keys+values, document order).
        #[test]
        fn oracle_matches_serde(v in proptest::string::string_regex("[ -~한-힣\\\\\"]{0,40}").unwrap(),
                                k in "[a-z]{1,8}") {
            // k == "z" collides with the fixed "z" key and collapses the map to a
            // single entry, making the hardcoded expectation below meaningless.
            prop_assume!(k != "z");
            let line = serde_json::to_vec(&serde_json::json!({ k.clone(): v.clone(), "z": [v.clone(), 1, null] })).unwrap();
            let spans = string_spans(&line).unwrap();
            let mine: Vec<String> = spans.iter().map(|s| decode_json_string(&line[s.start..s.end]).unwrap()).collect();
            prop_assert_eq!(mine, vec![k, v.clone(), "z".to_string(), v]);
        }
    }
}
