// Raspberry Pi Pico: blink an LED every 3 seconds and report over USB serial.
//
// Wiring (physical pin numbers on the Pico):
//   pin 27 (GP21) --[ resistor ]--> LED anode (long leg)
//   pin 28 (GND)  ------------------> LED cathode (short leg)
//
//   pneuma run      build + upload + open the monitor (default env: pico)
//
// First upload: hold BOOTSEL while plugging the Pico in, then run `pneuma upload`.
#include <Arduino.h>

const uint8_t LED_PIN = 21;           // GP21 = physical pin 27
const unsigned long HALF_PERIOD_MS = 3000;

void setup() {
  Serial.begin(115200);
  pinMode(LED_PIN, OUTPUT);
  delay(1500);                        // let the USB serial port come up
  Serial.println("Pico blink: LED on GP21, toggling every 3 s");
}

void loop() {
  static unsigned long count = 0;
  static bool on = false;
  on = !on;
  digitalWrite(LED_PIN, on ? HIGH : LOW);
  Serial.print("t=");
  Serial.print(millis() / 1000);
  Serial.print("s  LED ");
  Serial.print(on ? "ON " : "OFF");
  Serial.print("  toggle #");
  Serial.println(++count);
  delay(HALF_PERIOD_MS);
}
