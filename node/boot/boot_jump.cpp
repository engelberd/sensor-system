#include "boot/boot_jump.h"

#include <cstdint>

#include "boot/boot_config.h"
#include "hardware/address_mapped.h"
#include "hardware/platform_defs.h"
#include "hardware/sync.h"
#include "hardware/uart.h"
#include "pico/platform.h"

#if defined(__arm__) || defined(__thumb__)
#include "hardware/structs/scb.h"
#endif

namespace boot {
namespace {

using EntryFn = void (*)(void);

constexpr uintptr_t PPB_BASE_ADDR = 0xE000E000u;
constexpr uintptr_t SYSTICK_CTRL_ADDR = PPB_BASE_ADDR + 0x0010u;
constexpr uintptr_t SYSTICK_LOAD_ADDR = PPB_BASE_ADDR + 0x0014u;
constexpr uintptr_t SYSTICK_VAL_ADDR  = PPB_BASE_ADDR + 0x0018u;
constexpr uintptr_t NVIC_ICER_ADDR    = PPB_BASE_ADDR + 0x0180u;
constexpr uintptr_t NVIC_ICPR_ADDR    = PPB_BASE_ADDR + 0x0280u;

inline volatile uint32_t& reg32(uintptr_t addr) {
    return *reinterpret_cast<volatile uint32_t*>(addr);
}

[[noreturn]] void boot_halt_forever() {
    while (true) {
        __asm volatile("nop");
    }
}

bool read_slot_vectors(SlotId slot,
                       uint32_t& app_base,
                       uint32_t& initial_sp,
                       uint32_t& reset_handler) {
    const uint32_t offset = boot_slot_offset(slot);
    if (offset == 0u) {
        return false;
    }

    app_base = XIP_BASE + offset;
    const uint32_t* vectors = reinterpret_cast<const uint32_t*>(app_base);
    initial_sp = vectors[0];
    reset_handler = vectors[1];

    if ((initial_sp & 0xFF000000u) != 0x20000000u) {
        return false;
    }

    if ((reset_handler & 0xFF000000u) != 0x10000000u) {
        return false;
    }

    return (reset_handler & 0x1u) != 0u;
}

void prepare_cpu_for_vector_chainload() {
#if defined(__arm__) || defined(__thumb__)
    reg32(SYSTICK_CTRL_ADDR) = 0u;
    reg32(SYSTICK_LOAD_ADDR) = 0u;
    reg32(SYSTICK_VAL_ADDR) = 0u;

    constexpr size_t kNvicWordCount = (NUM_IRQS + 31u) / 32u;
    for (size_t i = 0; i < kNvicWordCount; ++i) {
        reg32(NVIC_ICER_ADDR + (i * sizeof(uint32_t))) = 0xFFFFFFFFu;
        reg32(NVIC_ICPR_ADDR + (i * sizeof(uint32_t))) = 0xFFFFFFFFu;
    }

    __asm volatile("dsb 0xF" : : : "memory");
    __asm volatile("isb 0xF" : : : "memory");

    __asm volatile("movs r0, #0" : : : "r0");
    __asm volatile("msr control, r0" : : : "memory", "r0");
    __asm volatile("msr psp, r0" : : : "memory", "r0");
    __asm volatile("msr psplim, r0" : : : "memory", "r0");
    __asm volatile("msr msplim, r0" : : : "memory", "r0");
    __asm volatile("isb 0xF" : : : "memory");
#endif
}

[[noreturn]] void vector_chainload(uint32_t app_base,
                                   uint32_t initial_sp,
                                   uint32_t reset_handler) {
    uart_deinit(uart0);

    const uint32_t irq_state = save_and_disable_interrupts();
    (void)irq_state;
    prepare_cpu_for_vector_chainload();

#if defined(__arm__) || defined(__thumb__)
    scb_hw->vtor = app_base;
    __asm volatile("msr msp, %0" : : "r"(initial_sp) : );
    restore_interrupts(0u);
#endif

    const EntryFn entry = reinterpret_cast<EntryFn>(reset_handler);
    entry();

    boot_halt_forever();
}

} // namespace

[[noreturn]] void boot_jump_to_slot(SlotId slot) {
    uint32_t app_base = 0u;
    uint32_t initial_sp = 0u;
    uint32_t reset_handler = 0u;
    if (!read_slot_vectors(slot, app_base, initial_sp, reset_handler)) {
        boot_halt_forever();
    }

    vector_chainload(app_base, initial_sp, reset_handler);
}

} // namespace boot
