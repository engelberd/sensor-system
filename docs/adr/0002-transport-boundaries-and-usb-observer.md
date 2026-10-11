# ADR 0002: Transport boundaries and the USB observer

Status: accepted

## Context

The production node currently has one RS485 host and one grant/commit data
delivery session. The application also enables USB stdio for startup text.
Adding unrestricted command or data access to that USB stream would couple
an observer to the production delivery cursor and could introduce unbounded USB
work into the acquisition runtime.

The existing RS485 implementation is intentionally deterministic: core 0 owns
sensor acquisition, core 1 owns transport work, buffers have fixed capacities,
and command responses are transmitted before deferred system effects are
executed. These properties take priority over adding another interface.

## Decision

RS485 remains the only production data-delivery endpoint. USB is a bounded,
best-effort observer endpoint built on the transport and query boundaries
described below.

### Command results

A command returns one `CommandResult` containing its status and optional
deferred effect. A deferred effect is therefore attached to the response that
must be transmitted before that effect executes. Controllers do not expose
global `take_*` state that another endpoint could consume.

### Transport diagnostics

Application command handling depends on `ITransportDiagnostics`, not on
`Rs485Port`. Physical link configuration and transmission remain responsibilities
of the RS485 endpoint and its port.

### Sensor ownership

All operations touching the ADXL355, including temperature reads, are
serialized by the acquisition owner. An observer must not access the sensor
driver directly.

### Runtime diagnostics

The diagnostic store records structured events. It does not print runtime text
to USB. Startup messages emitted before the acquisition loop are allowed.
Future text output, if needed, must drain diagnostic snapshots asynchronously
on core 1 and must never block core 0.

### USB observer capabilities

The USB observer depends on narrow read-only sources. Its initial implemented
capabilities are limited to identity/capabilities, observer statistics, and a
bounded latest-sample snapshot. The boundary permits later read-only access to:

- device identity, version, and capabilities;
- current configuration and status snapshots;
- acquisition, transport, and buffer statistics;
- diagnostic information, events, and fault snapshots;
- a bounded latest-sample snapshot designed specifically for observation.

The USB observer must not receive interfaces that permit:

- grant or commit operations;
- runtime configuration changes or flash writes;
- commissioning;
- self-test, restart, or bootloader entry;
- acquisition pause or resume.

The endpoint will process a bounded amount of RX and TX work per core-1
iteration. It will not wait for a connected host. Failure, disconnection, or
backpressure on USB may lose observer output but must not affect acquisition or
RS485 recording.

### Sample observation

The observer does not consume the production `DataPlane` queue. A
fixed-capacity, triple-buffered snapshot sink publishes the latest coherent sample batch and
timing metadata without a commit cursor or backpressure. Its producer path must
have fixed, measured execution time and must not wait for the USB reader.

## Consequences

- The current RS485 wire protocol and grant/commit semantics remain unchanged.
- A future endpoint cannot accidentally consume a deferred action created by a
  different command.
- Application status can report physical link diagnostics without depending on
  a concrete RS485 implementation.
- USB live diagnostic text is disabled at runtime; structured diagnostic
  commands remain the source of truth.
- Full recording over USB is outside the observer scope. It would require an
  explicit multi-consumer delivery design and a separate ADR.

## Verification gates

Before accepting the USB observer for deployment:

1. Host-side firmware tests cover command-response effects and serialized
   temperature access.
2. A full Pico SDK application target builds for the selected board revision.
3. Existing host tests pass without wire-format changes.
4. Hardware tests confirm unchanged RS485 recording and timing behavior.
5. USB work budgets are measured at the maximum supported sensor ODR.
