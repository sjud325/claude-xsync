use std::path::Path;

/// Files directly under `plugins/` root (config/manifest files) — NOT
/// directories. Directories (cache/, repos/, marketplaces/) are machine-built
/// artifacts and cross-OS poison; Claude Code reinstalls from these manifests.
/// [Spec §4 implementation-time note: verify reinstall behavior on a real
/// machine; adjust the include set there if needed.]
pub fn plugin_manifest_rels(plugins_dir: &Path) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(plugins_dir) else { return Vec::new(); };
    let mut rels: Vec<String> = rd
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| format!("plugins/{}", e.file_name().to_string_lossy()))
        .collect();
    rels.sort();
    rels
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn only_root_files_not_dirs() {
        let td = tempfile::tempdir().unwrap();
        let plugins = td.path().join("plugins");
        fs::create_dir_all(plugins.join("cache/some-plugin")).unwrap();
        fs::create_dir_all(plugins.join("repos")).unwrap();
        fs::write(plugins.join("config.json"), b"{}").unwrap();
        fs::write(plugins.join("installed.json"), b"{}").unwrap();
        fs::write(plugins.join("cache/some-plugin/build.node"), b"bin").unwrap();
        let rels = plugin_manifest_rels(&plugins);
        assert_eq!(rels, vec!["plugins/config.json".to_string(), "plugins/installed.json".to_string()]);
    }

    #[test]
    fn missing_plugins_dir_is_empty() {
        let td = tempfile::tempdir().unwrap();
        assert!(plugin_manifest_rels(&td.path().join("plugins")).is_empty());
    }
}
