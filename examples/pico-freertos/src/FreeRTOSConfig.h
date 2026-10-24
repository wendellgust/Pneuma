#ifndef FREERTOS_CONFIG_H
#define FREERTOS_CONFIG_H

#define configUSE_PREEMPTION                    1
#define configUSE_TICK_HOOK                     0
#define configUSE_IDLE_HOOK                     0
#define configCPU_CLOCK_HZ                      ( ( unsigned long ) 125000000 )
#define configTICK_RATE_HZ                      ( ( TickType_t ) 1000 )
#define configMAX_PRIORITIES                    5
#define configMINIMAL_STACK_SIZE                ( ( unsigned short ) 256 )
#define configTOTAL_HEAP_SIZE                   ( 64 * 1024 )
#define configMAX_TASK_NAME_LEN                 12
#define configUSE_16_BIT_TICKS                  0
#define configSUPPORT_DYNAMIC_ALLOCATION        1
#define configSUPPORT_STATIC_ALLOCATION         0
#define configNUMBER_OF_CORES                   1   /* single core: simplest */
#define configSUPPORT_PICO_SYNC_INTEROP         0   /* no pico_sync <-> task interop needed here */
#define configUSE_MUTEXES                       1
#define configUSE_TIMERS                        0

#define INCLUDE_vTaskDelay                      1
#define INCLUDE_vTaskDelete                     1
#define INCLUDE_vTaskDelayUntil                 1

#endif
