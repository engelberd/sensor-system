#pragma once

#include <cstdint>

#include "runtime/operation_mailbox.h"
#include "runtime/runtime_fault_latch.h"
#include "runtime/sensor_runtime.h"

namespace runtime {

struct Core0SensorRuntimeState {
    SensorRuntimeSnapshot sensor{};
    RuntimeFaultReason shared_fault = RuntimeFaultReason::None;
    OperationExecutorStatus last_command_status =
        OperationExecutorStatus::Idle;
    uint32_t poll_count = 0;
};

// Core-0 coordinator. A command transition is applied before the acquisition
// step, so Stop prevents further sampling in the same poll and Start may begin
// sampling immediately after a successful transition.
template <typename SensorRuntimeT, typename MailboxT>
class Core0SensorRuntime {
public:
    Core0SensorRuntime(SensorRuntimeT& sensor_runtime,
                       MailboxT& mailbox,
                       RuntimeFaultLatch& fault_latch)
        : sensor_runtime_(sensor_runtime), mailbox_(mailbox),
          fault_latch_(fault_latch) {}

    Core0SensorRuntime(const Core0SensorRuntime&) = delete;
    Core0SensorRuntime& operator=(const Core0SensorRuntime&) = delete;

    void poll() {
        if (fault_latch_.tripped()) {
            sensor_runtime_.fail(SensorStatus::InternalError);
        }
        last_command_status_ = executor_.poll(
            mailbox_,
            [this](const SensorRuntimeCommand& command) {
                return sensor_runtime_.handle(command);
            }
        );
        sensor_runtime_.poll();
        if (poll_count_ != UINT32_MAX) ++poll_count_;
    }

    Core0SensorRuntimeState state() const {
        return {
            sensor_runtime_.snapshot(),
            fault_latch_.reason(),
            last_command_status_,
            poll_count_,
        };
    }

    const auto& executor() const { return executor_; }

private:
    using Executor = OperationExecutor<
        MailboxT, SensorRuntimeCommand, SensorRuntimeResult>;

    SensorRuntimeT& sensor_runtime_;
    MailboxT& mailbox_;
    RuntimeFaultLatch& fault_latch_;
    Executor executor_{};
    OperationExecutorStatus last_command_status_ =
        OperationExecutorStatus::Idle;
    uint32_t poll_count_ = 0;
};

}  // namespace runtime
