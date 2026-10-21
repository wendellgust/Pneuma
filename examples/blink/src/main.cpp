// Pneuma example: blink the built-in LED and say hello over serial.
//
//   pneuma build           compile (default env: uno)
//   pneuma upload          flash the board
//   pneuma monitor         read the serial output (Ctrl+C quits)
//   pneuma run             all three in one go
//
// Other boards: add an env in pneuma.lua (e.g. nanoatmega328, esp32dev) and use `-e <env>`.
#include <Arduino.h>

#ifndef LED_BUILTIN
#define LED_BUILTIN 2  // generic ESP32 boards do not define it
#endif

const unsigned long BLINK_MS = 500;

void setup() {
  Serial.begin(115200);
  pinMode(LED_BUILTIN, OUTPUT);
  Serial.println("Pneuma blink example");
}

void loop() {
  static unsigned long count = 0;
  digitalWrite(LED_BUILTIN, HIGH);
  delay(BLINK_MS);
  digitalWrite(LED_BUILTIN, LOW);
  delay(BLINK_MS);
  Serial.print("blink #");
  Serial.println(++count);
}
