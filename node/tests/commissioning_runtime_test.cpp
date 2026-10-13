#include <cassert>

#include "runtime/commissioning_runtime.h"
#include "common/device_config.h"

namespace {

struct FakeConfig {
    DeviceConfig value{};
    bool save_ok = true;
    unsigned saves = 0;
    const DeviceConfig& current() const { return value; }
    void replace_device_config(const DeviceConfig& next) { value = next; }
    bool save() { ++saves; return save_ok; }
};

struct FakeBoot : runtime::ICommissioningBootSettings {
    bool ok = true;
    uint8_t last = 0;
    bool sync_node_address(uint8_t value) override {
        last = value;
        return ok;
    }
};

void assignment_is_durable_in_both_stores() {
    FakeConfig config;
    FakeBoot boot;
    runtime::CommissioningRuntime<FakeConfig> runtime(config, boot);
    const auto result = runtime.handle({7, 9, 42});
    assert(result.code == runtime::CommissioningResultCode::Assigned);
    assert(result.node_address == 42);
    assert(config.value.node_id == 42);
    assert(config.saves == 1);
    assert(boot.last == 42);
}

void assigned_node_and_invalid_address_are_untouched() {
    FakeConfig config;
    config.value.node_id = 8;
    FakeBoot boot;
    runtime::CommissioningRuntime<FakeConfig> runtime(config, boot);
    assert(runtime.handle({1, 1, 9}).code ==
           runtime::CommissioningResultCode::AlreadyAssigned);
    assert(config.saves == 0);
    config.value.node_id = 0;
    assert(runtime.handle({2, 1, 254}).code ==
           runtime::CommissioningResultCode::InvalidAddress);
    assert(config.saves == 0);
}

void storage_failure_restores_unassigned_state() {
    FakeConfig config;
    config.save_ok = false;
    FakeBoot boot;
    runtime::CommissioningRuntime<FakeConfig> runtime(config, boot);
    const auto result = runtime.handle({3, 1, 5});
    assert(result.code == runtime::CommissioningResultCode::StorageFailed);
    assert(config.value.node_id == 0);
    assert(boot.last == 0);
}

}  // namespace

int main() {
    assignment_is_durable_in_both_stores();
    assigned_node_and_invalid_address_are_untouched();
    storage_failure_restores_unassigned_state();
    return 0;
}
