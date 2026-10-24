// Pico + FreeRTOS: two tasks, one LED each.
//
// Wiring (physical pin numbers on the Pico, each LED through a resistor to GND):
//   pin 27 (GP21) --[ R ]--> green LED anode      pin 28 (GND) -> cathodes
//   pin 14 (GP10) --[ R ]--> red   LED anode      pin 13 (GND) -> cathodes
#include <stdio.h>
#include "pico/stdlib.h"
#include "FreeRTOS.h"
#include "task.h"

#define GREEN_PIN 21   // physical pin 27
#define RED_PIN   10   // physical pin 14

typedef struct {
    uint pin;
    const char *name;
    uint32_t half_period_ms;
} led_cfg_t;

static const led_cfg_t green = { GREEN_PIN, "green",  250 };   // fast
static const led_cfg_t red   = { RED_PIN,   "red",   1000 };   // slow

static void led_task(void *arg) {
    const led_cfg_t *cfg = arg;
    gpio_init(cfg->pin);
    gpio_set_dir(cfg->pin, GPIO_OUT);
    bool on = false;
    for (;;) {
        on = !on;
        gpio_put(cfg->pin, on);
        printf("[%lu ms] %s %s\n", (unsigned long)(xTaskGetTickCount() * portTICK_PERIOD_MS), cfg->name, on ? "ON" : "OFF");
        vTaskDelay(pdMS_TO_TICKS(cfg->half_period_ms));
    }
}

int main(void) {
    stdio_init_all();
    xTaskCreate(led_task, "green", 512, (void *)&green, 1, NULL);
    xTaskCreate(led_task, "red",   512, (void *)&red,   1, NULL);
    vTaskStartScheduler();
    for (;;) {}
}
