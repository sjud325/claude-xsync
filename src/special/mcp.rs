/// Extract the top-level `mcpServers` subtree from `~/.claude.json`,
/// serialized compact. `None` if the key is absent.
pub fn extract_mcp(claude_json: &str) -> anyhow::Result<Option<String>> {
    let v: serde_json::Value = serde_json::from_str(claude_json)?;
    Ok(v.get("mcpServers").map(serde_json::Value::to_string))
}

/// Replace/insert ONLY the `mcpServers` key; all sibling keys survive.
/// (serde re-serialize is acceptable here — this file is machine-local and
/// never round-trip-verified; `preserve_order` keeps key order stable.)
pub fn merge_mcp(claude_json: &str, mcp_subtree: &str) -> anyhow::Result<String> {
    let mut v: serde_json::Value = serde_json::from_str(claude_json)?;
    let subtree: serde_json::Value = serde_json::from_str(mcp_subtree)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!(".claude.json top level is not an object"))?;
    obj.insert("mcpServers".to_string(), subtree);
    Ok(v.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big_claude_json() -> String {
        // 70 machine-identity sibling keys + mcpServers subtree
        let mut obj = serde_json::Map::new();
        for i in 0..70 {
            obj.insert(format!("machineKey{i}"), serde_json::json!(i));
        }
        obj.insert("userID".into(), serde_json::json!("u-123"));
        obj.insert(
            "mcpServers".into(),
            serde_json::json!({"fs": {"command": "/Users/woong/bin/mcp-fs", "args": ["--root"]}}),
        );
        serde_json::to_string(&serde_json::Value::Object(obj)).unwrap()
    }

    #[test]
    fn extract_returns_only_subtree() {
        let subtree = extract_mcp(&big_claude_json()).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&subtree).unwrap();
        assert!(v.get("fs").is_some());
        assert!(v.get("machineKey0").is_none());
        assert!(v.get("userID").is_none());
    }

    #[test]
    fn extract_absent_is_none() {
        assert!(extract_mcp(r#"{"a":1}"#).unwrap().is_none());
    }

    #[test]
    fn merge_replaces_only_mcp_servers_and_keeps_siblings() {
        let other = r#"{"userID":"u-999","machineID":"m-1","mcpServers":{"old":{}}}"#;
        let merged = merge_mcp(other, r#"{"fs":{"command":"C:/Users/Loki/bin/mcp-fs"}}"#).unwrap();
        let v: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(v["userID"], "u-999");
        assert_eq!(v["machineID"], "m-1");
        assert!(v["mcpServers"].get("old").is_none());
        assert_eq!(v["mcpServers"]["fs"]["command"], "C:/Users/Loki/bin/mcp-fs");
    }

    #[test]
    fn merge_inserts_when_absent_and_roundtrip_idempotent() {
        let no_mcp = r#"{"userID":"u-1"}"#;
        let sub = r#"{"fs":{"command":"x"}}"#;
        let merged = merge_mcp(no_mcp, sub).unwrap();
        let extracted = extract_mcp(&merged).unwrap().unwrap();
        let again = merge_mcp(&merged, &extracted).unwrap();
        assert_eq!(merged, again); // idempotent
    }
}
