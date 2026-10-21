//! Small per-user memory kept in `~/.pneuma/state/`: which env last worked with which board.
//! Kept outside the project so build staging never touches it.

use std::path::{Path, PathBuf};

fn file(base: &Path, root: &Path) -> PathBuf {
    let key: String = root.display().to_string().chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    base.join("state").join(format!("{key}.txt"))
}

fn read(base: &Path, root: &Path) -> Vec<(String, String)> {
    std::fs::read_to_string(file(base, root))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('\t').map(|(a, b)| (a.to_string(), b.to_string())))
        .collect()
}

pub fn last_env_in(base: &Path, root: &Path, port_id: &str) -> Option<String> {
    read(base, root).into_iter().find(|(id, _)| id == port_id).map(|x| x.1)
}

pub fn set_last_env_in(base: &Path, root: &Path, port_id: &str, env: &str) {
    if port_id.is_empty() || port_id.contains(['\t', '\n']) || env.contains(['\t', '\n']) {
        return;
    }
    let mut all = read(base, root);
    all.retain(|(id, _)| id != port_id);
    all.push((port_id.to_string(), env.to_string()));
    let f = file(base, root);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, all.iter().map(|(a, b)| format!("{a}\t{b}\n")).collect::<String>());
}

pub fn last_env(root: &Path, port_id: &str) -> Option<String> {
    last_env_in(&crate::build::pneuma_home(), root, port_id)
}

pub fn set_last_env(root: &Path, port_id: &str, env: &str) {
    set_last_env_in(&crate::build::pneuma_home(), root, port_id, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_per_board_and_project() {
        let d = std::env::temp_dir().join(format!("pneuma_state_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let (a, b) = (Path::new("/proj/a"), Path::new("/proj/b"));
        assert_eq!(last_env_in(&d, a, "10c4:ea60:1"), None);
        set_last_env_in(&d, a, "10c4:ea60:1", "uno");
        set_last_env_in(&d, a, "1a86:7523:", "nano");
        set_last_env_in(&d, a, "10c4:ea60:1", "esp32");
        assert_eq!(last_env_in(&d, a, "10c4:ea60:1").as_deref(), Some("esp32"));
        assert_eq!(last_env_in(&d, a, "1a86:7523:").as_deref(), Some("nano"));
        assert_eq!(last_env_in(&d, b, "10c4:ea60:1"), None);
    }
}
