use crate::Result;
use serialport::SerialPortType;

#[derive(Debug)]
pub struct Port {
    pub usb: bool,
    pub name: String,
    pub desc: String,
    /// `vid:pid:serial` for USB ports (stable across replugs), empty otherwise.
    pub id: String,
}

/// `all = false` hides non-USB/non-Bluetooth ports (e.g. phantom `ttyS*`).
pub fn list(all: bool) -> Result<Vec<Port>> {
    Ok(serialport::available_ports()?
        .into_iter()
        .filter(|p| all || matches!(p.port_type, SerialPortType::UsbPort(_) | SerialPortType::BluetoothPort))
        .map(|p| {
            let usb = matches!(p.port_type, SerialPortType::UsbPort(_));
            let id = match &p.port_type {
                SerialPortType::UsbPort(u) => format!("{:04x}:{:04x}:{}", u.vid, u.pid, u.serial_number.clone().unwrap_or_default()),
                _ => String::new(),
            };
            let desc = match p.port_type {
                SerialPortType::UsbPort(u) => format!(
                    "USB {:04x}:{:04x} {}",
                    u.vid,
                    u.pid,
                    u.product.unwrap_or_default()
                ),
                SerialPortType::PciPort => "PCI".into(),
                SerialPortType::BluetoothPort => "Bluetooth".into(),
                SerialPortType::Unknown => "unknown".into(),
            };
            Port { usb, name: p.port_name, desc, id }
        })
        .collect())
}

/// Stable id (see `Port::id`) of a listed port by device name.
pub fn id_of(name: &str) -> Option<String> {
    list(true).ok()?.into_iter().find(|p| p.name == name).map(|p| p.id).filter(|i| !i.is_empty())
}

/// The single connected USB board, or a clear error.
pub fn auto_port() -> Result<String> {
    let usb: Vec<Port> = list(false)?.into_iter().filter(|p| p.usb).collect();
    match usb.len() {
        1 => Ok(usb.into_iter().next().unwrap().name),
        0 => Err(crate::Error::Build("nenhuma placa USB detectada".into())),
        _ => Err(crate::Error::Build(format!(
            "várias placas: {} — escolha com -p",
            usb.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Linux: processes (pid, command) that have `port` open. Empty elsewhere.
pub fn holders(port: &str) -> Vec<(u32, String)> {
    let Ok(target) = std::fs::canonicalize(port) else { return vec![] };
    let me = std::process::id();
    let mut out = vec![];
    for e in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()).filter(|p| *p != me) else { continue };
        let open = std::fs::read_dir(e.path().join("fd"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|f| std::fs::read_link(f.path()).is_ok_and(|l| l == target));
        if open {
            let cmd = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
            let cmd = String::from_utf8_lossy(&cmd).replace('\0', " ");
            out.push((pid, cmd.trim().to_string()));
        }
    }
    out
}

// ---------- pause protocol: `upload` asks running monitors to release the port ----------

fn pause_file(base: &std::path::Path, port: &str) -> std::path::PathBuf {
    let name = std::path::Path::new(port).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| port.replace('/', "_"));
    base.join("pause").join(name)
}

/// Held while uploading; monitors see it, close the port and reopen it when this is dropped.
pub struct PauseGuard(std::path::PathBuf);

impl Drop for PauseGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn pause_in(base: &std::path::Path, port: &str) -> PauseGuard {
    let f = pause_file(base, port);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&f, std::process::id().to_string());
    PauseGuard(f)
}

/// A marker only counts while the process that wrote it is alive (a killed upload can't pause forever).
fn is_paused_in(base: &std::path::Path, port: &str) -> bool {
    let f = pause_file(base, port);
    let Ok(pid) = std::fs::read_to_string(&f) else { return false };
    // /proc only exists on Linux; elsewhere trust the marker (upload removes it on drop).
    let alive = pid.trim().parse::<u32>().is_ok_and(|p| !cfg!(target_os = "linux") || std::path::Path::new(&format!("/proc/{p}")).exists());
    if !alive {
        let _ = std::fs::remove_file(&f);
    }
    alive
}

pub fn pause(port: &str) -> PauseGuard {
    pause_in(&crate::build::pneuma_home(), port)
}

pub fn is_paused(port: &str) -> bool {
    is_paused_in(&crate::build::pneuma_home(), port)
}

/// Wait up to `timeout` for everybody to close `port`; returns who still holds it.
pub fn wait_released(port: &str, timeout: std::time::Duration) -> Vec<(u32, String)> {
    let end = std::time::Instant::now() + timeout;
    loop {
        let h = holders(port);
        if h.is_empty() || std::time::Instant::now() >= end {
            return h;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_marker_lifecycle_and_stale_pid() {
        let d = std::env::temp_dir().join(format!("pneuma_pause_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(!is_paused_in(&d, "/dev/ttyUSB9"));
        {
            let _g = pause_in(&d, "/dev/ttyUSB9");
            assert!(is_paused_in(&d, "/dev/ttyUSB9"));
            assert!(!is_paused_in(&d, "/dev/ttyUSB8"));
        }
        assert!(!is_paused_in(&d, "/dev/ttyUSB9"));
        std::fs::create_dir_all(d.join("pause")).unwrap();
        std::fs::write(d.join("pause/ttyUSB9"), "4294967").unwrap();
        assert!(!is_paused_in(&d, "/dev/ttyUSB9"));
        assert!(!d.join("pause/ttyUSB9").exists());
    }
}
