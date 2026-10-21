//! Open a command in a NEW terminal window (so the monitor can sit next to the editor).

use crate::{Error, Result};
use std::process::{Command, Stdio};

/// (binary, args placed before the command, env var that proves we're running inside it)
const TERMS: &[(&str, &[&str], &str)] = &[
    ("kitty", &[], "KITTY_WINDOW_ID"),
    ("alacritty", &["-e"], "ALACRITTY_SOCKET"),
    ("wezterm", &["start", "--"], "WEZTERM_PANE"),
    ("foot", &[], "FOOT_SERVER_SOCKET"),
    ("gnome-terminal", &["--"], "GNOME_TERMINAL_SCREEN"),
    ("konsole", &["-e"], "KONSOLE_VERSION"),
    ("xfce4-terminal", &["-x"], ""),
    ("tilix", &["-e"], "TILIX_ID"),
    ("terminator", &["-x"], ""),
    ("x-terminal-emulator", &["-e"], ""),
    ("xterm", &["-e"], ""),
];

#[derive(Debug, PartialEq, Eq)]
pub enum Launcher {
    Tmux,
    MacTerminal,
    Term(String),
}

fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub fn shell_join(argv: &[String]) -> String {
    argv.iter().map(|a| shell_quote(a)).collect::<Vec<_>>().join(" ")
}

pub fn detect_with(env: impl Fn(&str) -> Option<String>, has_bin: impl Fn(&str) -> bool, macos: bool) -> Option<Launcher> {
    let named = |n: &str| TERMS.iter().find(|t| t.0 == n).map(|t| t.0.to_string());
    if let Some(t) = env("PNEUMA_TERMINAL").or_else(|| env("BMU_TERMINAL")).filter(|t| !t.is_empty()) {
        return Some(Launcher::Term(t));
    }
    if macos {
        return Some(Launcher::MacTerminal);
    }
    if env("TMUX").is_some_and(|v| !v.is_empty()) && has_bin("tmux") {
        return Some(Launcher::Tmux);
    }
    // The terminal we are running in first, then $TERMINAL, then whatever is installed.
    if let Some(t) = TERMS.iter().find(|t| !t.2.is_empty() && env(t.2).is_some()).and_then(|t| named(t.0)) {
        if has_bin(&t) {
            return Some(Launcher::Term(t));
        }
    }
    if let Some(t) = env("TERMINAL").filter(|t| !t.is_empty() && has_bin(t)) {
        return Some(Launcher::Term(t));
    }
    TERMS.iter().map(|t| t.0).find(|b| has_bin(b)).map(|b| Launcher::Term(b.to_string()))
}

/// Full argv (program first) that runs `argv` inside the launcher's terminal.
pub fn launch_argv(l: &Launcher, argv: &[String]) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    match l {
        Launcher::Tmux => v.extend(["tmux".into(), "split-window".into(), "-h".into(), shell_join(argv)]),
        Launcher::MacTerminal => {
            let script = format!("tell application \"Terminal\" to do script \"{}\"", shell_join(argv).replace('\\', "\\\\").replace('"', "\\\""));
            v.extend(["osascript".into(), "-e".into(), script]);
        }
        Launcher::Term(t) => {
            let pre: &[&str] = TERMS.iter().find(|x| x.0 == t.rsplit('/').next().unwrap_or(t)).map(|x| x.1).unwrap_or(&["-e"]);
            v.push(t.clone());
            v.extend(pre.iter().map(|s| s.to_string()));
            v.extend(argv.iter().cloned());
        }
    }
    v
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// Run `argv` (with `dir` as working directory) in a new terminal window, detached.
pub fn open(dir: &std::path::Path, argv: &[String]) -> Result<String> {
    let l = detect_with(|k| std::env::var(k).ok(), on_path, cfg!(target_os = "macos")).ok_or_else(|| {
        Error::Build("nenhum emulador de terminal encontrado; defina PNEUMA_TERMINAL (ex.: kitty)".into())
    })?;
    // `sh -c 'cd dir && exec cmd'` so the new window starts in the project, whatever its default cwd.
    let mut wrapped: Vec<String> = ["sh", "-c", "cd \"$1\" && shift && exec \"$@\"", "sh"].map(String::from).to_vec();
    wrapped.push(dir.display().to_string());
    wrapped.extend(argv.iter().cloned());
    let full = launch_argv(&l, &wrapped);
    let mut c = Command::new(&full[0]);
    c.args(&full[1..]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    c.spawn().map_err(|e| Error::Build(format!("{}: {e}", full[0])))?;
    Ok(full[0].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn det(env: &[(&str, &str)], bins: &[&str], mac: bool) -> Option<Launcher> {
        let m: HashMap<String, String> = env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        detect_with(|k| m.get(k).cloned(), |b| bins.contains(&b), mac)
    }
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn detection_priority() {
        assert_eq!(det(&[("PNEUMA_TERMINAL", "foot")], &[], false), Some(Launcher::Term("foot".into())));
        assert_eq!(det(&[("TMUX", "/tmp/x")], &["tmux", "kitty"], false), Some(Launcher::Tmux));
        assert_eq!(det(&[("KITTY_WINDOW_ID", "1")], &["kitty", "xterm"], false), Some(Launcher::Term("kitty".into())));
        assert_eq!(det(&[("TERMINAL", "alacritty")], &["alacritty", "xterm"], false), Some(Launcher::Term("alacritty".into())));
        assert_eq!(det(&[], &["xterm"], false), Some(Launcher::Term("xterm".into())));
        assert_eq!(det(&[], &[], false), None);
        assert_eq!(det(&[], &[], true), Some(Launcher::MacTerminal));
    }

    #[test]
    fn per_terminal_command_shape() {
        let c = s(&["pneuma", "monitor"]);
        assert_eq!(launch_argv(&Launcher::Term("kitty".into()), &c), ["kitty", "pneuma", "monitor"]);
        assert_eq!(launch_argv(&Launcher::Term("gnome-terminal".into()), &c), ["gnome-terminal", "--", "pneuma", "monitor"]);
        assert_eq!(launch_argv(&Launcher::Term("xfce4-terminal".into()), &c), ["xfce4-terminal", "-x", "pneuma", "monitor"]);
        assert_eq!(launch_argv(&Launcher::Term("wezterm".into()), &c), ["wezterm", "start", "--", "pneuma", "monitor"]);
        assert_eq!(launch_argv(&Launcher::Term("/usr/bin/xterm".into()), &c), ["/usr/bin/xterm", "-e", "pneuma", "monitor"]);
        assert_eq!(launch_argv(&Launcher::Tmux, &s(&["a b", "c"])), ["tmux", "split-window", "-h", "'a b' c"]);
        assert!(launch_argv(&Launcher::MacTerminal, &c)[2].contains("do script"));
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_join(&s(&["a", "it's", "x y", ""])), "a 'it'\\''s' 'x y' ''");
    }
}
