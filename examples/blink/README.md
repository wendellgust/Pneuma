# Blink

The smallest useful test for a Pneuma setup: blinks the built-in LED twice a second and prints `blink #N` over serial at 115200 baud.

```
cd examples/blink
pneuma setup        # once: AVR toolchain, no arduino-cli needed
pneuma build        # compiles for the Uno
pneuma upload       # flashes the board (close any monitor first, or let Pneuma pause it)
pneuma monitor      # shows: blink #1, blink #2, ...
```

The LED on pin 13 should blink. For another board, pick an env from `pneuma.lua` with `-e`.
