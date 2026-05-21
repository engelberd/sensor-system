#pragma once
#include <cstdint>

/*
============================================================
ADXL355 / ADXL354 REGISTER MAP
============================================================
Źródło: datasheet Analog Devices

Konwencja:
- ADDR = adres rejestru
- *_MASK = maska bitowa
- *_POS = pozycja bitu (do przesuwania)
============================================================
*/

namespace ADXL355
{


    constexpr uint8_t RANGE_MASK = 0x03;

    constexpr uint8_t POWER_STANDBY_MASK  = 1 << 0;
    constexpr uint8_t POWER_TEMP_OFF_MASK = 1 << 1;
    constexpr uint8_t POWER_DRDY_OFF_MASK = 1 << 2;

    constexpr uint8_t FIFO_ENTRIES_MASK = 0x7F;

    constexpr uint8_t STATUS_DATA_RDY_MASK = 1 << 0;
    constexpr uint8_t STATUS_FIFO_FULL_MASK = 1 << 1;
    constexpr uint8_t STATUS_FIFO_OVR_MASK  = 1 << 2;
    constexpr uint8_t STATUS_ACTIVITY_MASK  = 1 << 3;
    constexpr uint8_t STATUS_NVM_BUSY_MASK  = 1 << 4;

    constexpr uint8_t SHADOW_REG1 = 0x50;
    constexpr uint8_t SHADOW_REG_COUNT = 5;
    // =========================
    // DEVICE ID REGISTERS
    // =========================
    constexpr uint8_t DEVID_AD   = 0x00; // Expected: 0xAD (Analog Devices ID)
    constexpr uint8_t DEVID_MST  = 0x01; // Expected: 0x1D (MEMS ID)
    constexpr uint8_t PARTID     = 0x02; // Expected: 0xED (Device ID)
    constexpr uint8_t REVID      = 0x03; // Revision ID

    // =========================
    // STATUS REGISTER (0x04)
    // =========================
    constexpr uint8_t STATUS     = 0x04;

    // STATUS bits
    constexpr uint8_t STATUS_DATA_RDY = 1 << 0; // New XYZ sample ready
    constexpr uint8_t STATUS_FIFO_FULL = 1 << 1;
    constexpr uint8_t STATUS_FIFO_OVR  = 1 << 2;
    constexpr uint8_t STATUS_ACTIVITY  = 1 << 3;
    constexpr uint8_t STATUS_NVM_BUSY  = 1 << 4;

    // =========================
    // FIFO
    // =========================
    constexpr uint8_t FIFO_ENTRIES = 0x05; // number of samples in FIFO (0–96)
    constexpr uint8_t FIFO_DATA    = 0x11; // reading pops FIFO

    // =========================
    // TEMPERATURE
    // =========================
    constexpr uint8_t TEMP2 = 0x06; // MSB [11:8]
    constexpr uint8_t TEMP1 = 0x07; // LSB [7:0]

    // =========================
    // ACCELERATION DATA (20-bit, left-justified)
    // =========================
    constexpr uint8_t XDATA3 = 0x08; // bits [19:12]
    constexpr uint8_t XDATA2 = 0x09; // bits [11:4]
    constexpr uint8_t XDATA1 = 0x0A; // bits [3:0]

    constexpr uint8_t YDATA3 = 0x0B;
    constexpr uint8_t YDATA2 = 0x0C;
    constexpr uint8_t YDATA1 = 0x0D;

    constexpr uint8_t ZDATA3 = 0x0E;
    constexpr uint8_t ZDATA2 = 0x0F;
    constexpr uint8_t ZDATA1 = 0x10;

    // =========================
    // OFFSET CALIBRATION
    // =========================
    constexpr uint8_t OFFSET_X_H = 0x1E;
    constexpr uint8_t OFFSET_X_L = 0x1F;

    constexpr uint8_t OFFSET_Y_H = 0x20;
    constexpr uint8_t OFFSET_Y_L = 0x21;

    constexpr uint8_t OFFSET_Z_H = 0x22;
    constexpr uint8_t OFFSET_Z_L = 0x23;

    // =========================
    // ACTIVITY DETECTION
    // =========================
    constexpr uint8_t ACT_EN       = 0x24;
    constexpr uint8_t ACT_THRESH_H = 0x25;
    constexpr uint8_t ACT_THRESH_L = 0x26;
    constexpr uint8_t ACT_COUNT    = 0x27;

    // =========================
    // FILTER REGISTER (0x28)
    // =========================
    constexpr uint8_t FILTER = 0x28;

    // HPF (bits 6:4)
    constexpr uint8_t HPF_CORNER_POS = 4;
    constexpr uint8_t HPF_CORNER_MASK = 0b01110000;

    // ODR + LPF (bits 3:0)
    constexpr uint8_t ODR_LPF_MASK = 0b00001111;

    // przykładowe ustawienia:
    constexpr uint8_t ODR_4000HZ = 0x00;
    constexpr uint8_t ODR_2000HZ = 0x01;
    constexpr uint8_t ODR_1000HZ = 0x02;
    constexpr uint8_t ODR_500HZ  = 0x03;
    constexpr uint8_t ODR_250HZ  = 0x04;
    constexpr uint8_t ODR_125HZ  = 0x05;
    constexpr uint8_t ODR_62_5HZ = 0x06;

    // =========================
    // FIFO CONFIG
    // =========================
    constexpr uint8_t FIFO_SAMPLES = 0x29;
    constexpr uint8_t FIFO_SAMPLES_MASK = 0x7F;
    constexpr uint8_t FIFO_SAMPLES_MIN = 1;
    constexpr uint8_t FIFO_SAMPLES_MAX = 96;

    // =========================
    // INTERRUPTS
    // =========================
    constexpr uint8_t INT_MAP = 0x2A;

    constexpr uint8_t INT_RDY_EN1  = 1 << 0;
    constexpr uint8_t INT_FULL_EN1 = 1 << 1;
    constexpr uint8_t INT_OVR_EN1  = 1 << 2;
    constexpr uint8_t INT_ACT_EN1  = 1 << 3;
    constexpr uint8_t INT_RDY_EN2  = 1 << 4;
    constexpr uint8_t INT_FULL_EN2 = 1 << 5;
    constexpr uint8_t INT_OVR_EN2  = 1 << 6;
    constexpr uint8_t INT_ACT_EN2  = 1 << 7;

    // =========================
    // SYNC
    // =========================
    constexpr uint8_t SYNC = 0x2B;

    constexpr uint8_t SYNC_EXT_CLK  = 1 << 2;
    constexpr uint8_t SYNC_EXT_SYNC = 0b11;

    // =========================
    // RANGE + INTERFACE (0x2C)
    // =========================
    constexpr uint8_t RANGE = 0x2C;

    constexpr uint8_t RANGE_2G = 0x01;
    constexpr uint8_t RANGE_4G = 0x02;
    constexpr uint8_t RANGE_8G = 0x03;
    constexpr uint8_t RANGE_INT_POL_MASK = 1 << 6;
    constexpr uint8_t RANGE_I2C_HS_MASK = 1 << 7;

    // =========================
    // POWER CONTROL (0x2D)
    // =========================
    constexpr uint8_t POWER_CTL = 0x2D;

    constexpr uint8_t POWER_STANDBY = 1 << 0;
    constexpr uint8_t POWER_TEMP_OFF = 1 << 1;
    constexpr uint8_t POWER_DRDY_OFF = 1 << 2;

    // =========================
    // SELF TEST (0x2E)
    // =========================
    constexpr uint8_t SELF_TEST = 0x2E;

    constexpr uint8_t SELF_TEST_ST1 = 1 << 0;
    constexpr uint8_t SELF_TEST_ST2 = 1 << 1;

    // =========================
    // RESET (0x2F)
    // =========================
    constexpr uint8_t RESET = 0x2F;

    constexpr uint8_t RESET_CODE = 0x52; // write this to reset device
}
