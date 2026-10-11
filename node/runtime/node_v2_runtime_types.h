#pragma once

#include "config/node_v2_runtime_profile.h"
#include "measurement/measurement_ingress_sink.h"
#include "protocol_v3/lifecycle_handler.h"
#include "protocol_v3/transport_endpoint.h"
#include "runtime/core1_measurement_runtime.h"
#include "runtime/operation_mailbox.h"

namespace node_v2 {

using Profile = RuntimeCapacityProfile;
using IngressQueue = runtime::SpscQueue<
    measurement::IngressEvent,
    Profile::kIngressEvents>;
using SampleIngress = measurement::SampleIngress<Profile::kIngressEvents>;
using MeasurementSink = measurement::MeasurementIngressSink<SampleIngress>;
using OperationMailbox = runtime::OperationMailbox<
    runtime::SensorRuntimeCommand,
    runtime::SensorRuntimeResult,
    Profile::kCommandMailboxEntries,
    Profile::kResultMailboxEntries>;
using SystemOperationMailbox = runtime::OperationMailbox<
    runtime::SystemOperationCommand,
    runtime::SensorRuntimeResult,
    Profile::kCommandMailboxEntries,
    Profile::kResultMailboxEntries>;
using Core1MeasurementRuntime = runtime::Core1MeasurementRuntime<
    Profile::kIngressEvents,
    Profile::kJournalSamples,
    Profile::kJournalGaps,
    Profile::kMaxChunkSamples>;
using LifecycleService = ProtocolV3::LifecycleServiceHandler<
    OperationMailbox,
    SystemOperationMailbox,
    Profile::kLifecycleResultCacheEntries>;

template <typename Port>
using TransportEndpoint = ProtocolV3::TransportEndpoint<
    Port,
    Profile::kResponseOutboxEntries>;

static_assert(
    Profile::kMaxChunkSamples <= ProtocolV3::kMaxWireChunkSamples,
    "runtime chunk exceeds protocol payload");
static_assert(
    Profile::kIngressDrainBudgetPerPoll <= Profile::kIngressEvents,
    "drain budget exceeds ingress capacity");
static_assert(
    sizeof(IngressQueue) <= 8u * 1024u,
    "provisional ingress queue exceeds RAM allocation");
static_assert(
    Core1MeasurementRuntime::Journal::storage_bytes() <= 72u * 1024u,
    "provisional journal exceeds RAM allocation");

}  // namespace node_v2
