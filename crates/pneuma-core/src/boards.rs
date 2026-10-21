//! Built-in board registry. Initial scope: ESP32, RP2040, Arduino AVR.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Esp32,
    Esp8266,
    Rp2040,
    Avr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Uploader {
    Esptool,
    Picotool,
    Avrdude,
}

#[derive(Debug, Clone, Copy)]
pub struct Board {
    pub id: &'static str,
    pub name: &'static str,
    pub family: Family,
    pub mcu: &'static str,
    pub f_cpu_hz: u32,
    pub upload_speed: u32,
    pub uploader: Uploader,
    /// arduino-cli fully-qualified board name (build backend).
    pub fqbn: &'static str,
}

macro_rules! b {
    ($id:expr, $name:expr, $fam:ident, $mcu:expr, $cpu:expr, $speed:expr, $up:ident, $fqbn:expr) => {
        Board { id: $id, name: $name, family: Family::$fam, mcu: $mcu, f_cpu_hz: $cpu, upload_speed: $speed, uploader: Uploader::$up, fqbn: $fqbn }
    };
}

pub const BOARDS: &[Board] = &[
    b!("esp32dev", "Espressif ESP32 Dev Module", Esp32, "esp32", 240_000_000, 921_600, Esptool, "esp32:esp32:esp32"),
    b!("esp32-s3-devkitc-1", "Espressif ESP32-S3-DevKitC-1", Esp32, "esp32s3", 240_000_000, 921_600, Esptool, "esp32:esp32:esp32s3"),
    b!("esp32-c3-devkitm-1", "Espressif ESP32-C3-DevKitM-1", Esp32, "esp32c3", 160_000_000, 921_600, Esptool, "esp32:esp32:esp32c3"),
    b!("nodemcuv2", "NodeMCU 1.0 (ESP8266)", Esp8266, "esp8266", 80_000_000, 921_600, Esptool, "esp8266:esp8266:nodemcuv2"),
    b!("d1_mini", "WeMos D1 mini (ESP8266)", Esp8266, "esp8266", 80_000_000, 921_600, Esptool, "esp8266:esp8266:d1_mini"),
    b!("pico", "Raspberry Pi Pico", Rp2040, "rp2040", 133_000_000, 0, Picotool, "rp2040:rp2040:rpipico"),
    b!("rpipicow", "Raspberry Pi Pico W", Rp2040, "rp2040", 133_000_000, 0, Picotool, "rp2040:rp2040:rpipicow"),
    b!("uno", "Arduino Uno", Avr, "atmega328p", 16_000_000, 115_200, Avrdude, "arduino:avr:uno"),
    b!("nanoatmega328", "Arduino Nano ATmega328", Avr, "atmega328p", 16_000_000, 57_600, Avrdude, "arduino:avr:nano:cpu=atmega328old"),
    b!("nanoatmega328new", "Arduino Nano ATmega328 (new bootloader)", Avr, "atmega328p", 16_000_000, 115_200, Avrdude, "arduino:avr:nano:cpu=atmega328"),
    b!("megaatmega2560", "Arduino Mega 2560", Avr, "atmega2560", 16_000_000, 115_200, Avrdude, "arduino:avr:mega:cpu=atmega2560"),
];

pub fn find(id: &str) -> Option<&'static Board> {
    BOARDS.iter().find(|b| b.id == id)
}

impl Board {
    /// `vendor:arch` part of the FQBN, e.g. `esp32:esp32`.
    pub fn core(&self) -> &'static str {
        let mut it = self.fqbn.splitn(3, ':');
        let (v, a) = (it.next().unwrap_or(""), it.next().unwrap_or(""));
        &self.fqbn[..v.len() + 1 + a.len()]
    }

    /// Third-party package index needed for this core, if any.
    pub fn index_url(&self) -> Option<&'static str> {
        match self.family {
            Family::Esp32 => Some("https://espressif.github.io/arduino-esp32/package_esp32_index.json"),
            Family::Esp8266 => Some("https://arduino.esp8266.com/stable/package_esp8266com_index.json"),
            Family::Rp2040 => Some("https://github.com/earlephilhower/arduino-pico/releases/download/global/package_rp2040_index.json"),
            Family::Avr => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cores_and_fqbn() {
        assert_eq!(find("esp32dev").unwrap().core(), "esp32:esp32");
        assert_eq!(find("uno").unwrap().core(), "arduino:avr");
        assert_eq!(find("nodemcuv2").unwrap().core(), "esp8266:esp8266");
        assert_eq!(find("nanoatmega328").unwrap().upload_speed, 57_600);
        assert_eq!(find("nanoatmega328new").unwrap().upload_speed, 115_200);
        assert_eq!(find("rpipicow").unwrap().core(), "rp2040:rp2040");
        assert!(BOARDS.iter().all(|b| b.fqbn.matches(':').count() >= 2));
    }
}
