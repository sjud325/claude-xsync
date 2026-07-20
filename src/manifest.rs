use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Encrypted index at the repo root (`manifest.age`): portable path → entry.
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub last_push_device: String,
    pub last_push_ts: u64,
    /// Every device name that has ever pushed — lets init warn when a new
    /// machine picks a name that is already taken (duplicates silently
    /// disable Guard A). Best-effort: absent in pre-0.1.15 manifests, and a
    /// mixed-version fleet may drop it (old versions rewrite without it).
    #[serde(default)]
    pub devices: std::collections::BTreeSet<String>,
    pub entries: BTreeMap<String, Entry>, // key = portable path
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub object: String,
    pub chunks: u32,
    pub plaintext_hash: String,
    pub size: u64,
    pub mode: EntryMode,
    /// Source file's modification time (unix secs) — restored on pull so
    /// `--resume` ordering survives the sync. Absent in pre-0.1.5 manifests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub enum EntryMode {
    Transformed,
    Verbatim,
}

pub const CHUNK_SIZE: usize = 90 * 1024 * 1024;

/// Repo-relative file names for an object: single `.age` or `.age.N` chunks.
pub fn chunk_paths(object: &str, chunks: u32) -> Vec<String> {
    if chunks <= 1 {
        vec![format!("objects/{object}.age")]
    } else {
        (0..chunks)
            .map(|i| format!("objects/{object}.age.{i}"))
            .collect()
    }
}

pub fn split_chunks(sealed: &[u8]) -> Vec<&[u8]> {
    if sealed.is_empty() {
        return vec![&[]];
    }
    sealed.chunks(CHUNK_SIZE).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "projects/${HOME}-ws-app/s.jsonl".to_string(),
            Entry {
                object: "abcd".into(),
                chunks: 1,
                plaintext_hash: "ff".into(),
                size: 42,
                mode: EntryMode::Transformed,
                mtime: Some(1_700_000_000),
            },
        );
        let m = Manifest {
            version: 1,
            last_push_device: "mac".into(),
            last_push_ts: 1234,
            devices: Default::default(),
            entries,
        };
        let json = serde_json::to_vec(&m).unwrap();
        let back: Manifest = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.last_push_device, "mac");
        assert_eq!(back.last_push_ts, 1234);
        let e = &back.entries["projects/${HOME}-ws-app/s.jsonl"];
        assert_eq!(e.object, "abcd");
        assert!(e.mode == EntryMode::Transformed);
    }

    #[test]
    fn devices_registry_defaults_empty_and_roundtrips() {
        // pre-0.1.15 manifests carry no devices field
        let legacy = r#"{"version":1,"last_push_device":"mac","last_push_ts":1,"entries":{}}"#;
        let m: Manifest = serde_json::from_str(legacy).unwrap();
        assert!(m.devices.is_empty());

        let m2 = Manifest {
            version: 1,
            last_push_device: "mac".into(),
            last_push_ts: 1,
            devices: ["mac", "win"].iter().map(|s| s.to_string()).collect(),
            entries: Default::default(),
        };
        let back: Manifest = serde_json::from_slice(&serde_json::to_vec(&m2).unwrap()).unwrap();
        assert!(back.devices.contains("mac") && back.devices.contains("win"));
    }

    #[test]
    fn chunking_split_and_paths() {
        let dummy = vec![7u8; 200 * 1024 * 1024]; // 200 MiB
        let chunks = split_chunks(&dummy);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), CHUNK_SIZE);
        assert_eq!(chunks[1].len(), CHUNK_SIZE);
        let concat: Vec<u8> = chunks.concat();
        assert_eq!(concat, dummy);

        assert_eq!(chunk_paths("ab", 1), vec!["objects/ab.age".to_string()]);
        assert_eq!(
            chunk_paths("ab", 3),
            vec![
                "objects/ab.age.0".to_string(),
                "objects/ab.age.1".to_string(),
                "objects/ab.age.2".to_string()
            ]
        );
    }
}
