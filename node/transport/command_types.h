#pragma once

#include <cstdint>

enum class CommandType : uint8_t {
    Ping = 0x01,
    Restart = 0x02,
    EnterBootloader = 0x03,
    GetVersion = 0x04,
    RunSelfTest = 0x05,

    GetConfig = 0x20,
    SetNodeId = 0x21,
    SetOdr = 0x22,
    SetRange = 0x23,
    SetOffsets = 0x24,
    SetFifoWatermark = 0x25,
    SaveConfig = 0x26,
    LoadConfig = 0x27,
    ResetConfigToDefaults = 0x28,
    SetBaudRate = 0x29,
    CommissionDiscover = 0x2A,
    CommissionAssignNodeId = 0x2B,
    SetHighPass = 0x2C,

    GetStatus = 0x40,
    GetTemperature = 0x41,
    GetBufferState = 0x42,
    GetStats = 0x43,
    GetDiagnosticInfo = 0x44,
    GetFaultSnapshot = 0x45,
    ReadDiagnosticEvents = 0x46,
    ClearDiagnosticEvents = 0x47,
    GetPersistentDiagnosticRecord = 0x48,
    ClearPersistentDiagnosticRecord = 0x49,
    GetCapabilities = 0x4A,
    TimeSync = 0x4B,

    ReadLatest = 0x50,
    ReadFromSeq = 0x51,
    GrantBurstRead = 0x52,
    CommitReadUpTo = 0x53,
};
