# Pico blink

Blinks an LED on **GP21** (physical pin 27) every 3 s and prints the state over USB serial.

| Pico pin | Goes to |
|---|---|
| 27 (GP21) | resistor, then LED long leg |
| 28 (GND) | LED short leg |

```
pneuma setup --arduino-cli   # once
pneuma run                   # build, upload, monitor
```

First upload: hold **BOOTSEL** while plugging in the USB cable, then run `pneuma upload`.
