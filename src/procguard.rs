use std::path::Path;
use sysinfo::{Pid, System};

/// Live Claude Code instances: `sessions/*.json` records a pid registry —
/// return the pids that are still alive on this machine.
pub fn claude_running(claude_dir: &Path) -> Vec<u32> {
    let sessions = claude_dir.join("sessions");
    let Ok(rd) = std::fs::read_dir(&sessions) else { return Vec::new(); };
    let mut recorded = Vec::new();
    for entry in rd.flatten() {
        if entry.path().extension().map(|e| e == "json") != Some(true) {
            continue;
        }
        let Ok(raw) = std::fs::read(entry.path()) else { continue; };
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&raw) else { continue; };
        if let Some(pid) = v.get("pid").and_then(|p| p.as_u64()) {
            recorded.push(pid as u32);
        }
    }
    let mut sys = System::new();
    recorded
        .into_iter()
        .filter(|&pid| {
            sys.refresh_process(Pid::from_u32(pid));
            sys.process(Pid::from_u32(pid)).is_some()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_alive_pid_and_ignores_dead() {
        let td = tempfile::tempdir().unwrap();
        let sessions = td.path().join("sessions");
        fs::create_dir_all(&sessions).unwrap();
        let me = std::process::id();
        fs::write(sessions.join("a.json"), format!(r#"{{"pid":{me},"entrypoint":"cli"}}"#)).unwrap();
        fs::write(sessions.join("b.json"), r#"{"pid":999999,"entrypoint":"cli"}"#).unwrap();
        let alive = claude_running(td.path());
        assert!(alive.contains(&me));
        assert!(!alive.contains(&999999));
    }

    #[test]
    fn no_sessions_dir_means_nothing_running() {
        let td = tempfile::tempdir().unwrap();
        assert!(claude_running(td.path()).is_empty());
    }
}
