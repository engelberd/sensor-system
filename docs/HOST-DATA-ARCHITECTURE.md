# Host data architecture

## Purpose

The host has two deliberately different persistence products:

- **Capture** is the crash-safe, diagnostic recording retained for 30 days.
- **Archive** is the compact, validated measurement record retained long-term.

Derived products such as resampled or cross-sensor synchronized data are not
source records and can always be rebuilt from Archive data.

## Identity

Until permanent hardware identifiers are commissioned, transport channels are
numbered `1` through `8` and their attached sensors are labelled `A` through
`H`. A transport `node_address` is not a sensor identity. The data model keeps
`channel_id`, `sensor_label`, future `sensor_id`, and `node_address` separate.

## Dependency rule

The capture code is split into the following layers:

1. `host.recorder.model` defines records and runtime state. It does not know a
   file format or windowing policy.
2. `host.recorder.decoder` translates the wire representation into domain
   records. It does not write files.
3. `host.recorder.ports` defines the output contract consumed by the recorder.
4. `host.recorder.capture_v1` serializes the canonical Capture v1 journal.
   Legacy v5 serialization remains isolated in `capture_writers` during the
   staged rollout.
5. `host.recorder.capture_windowing` owns exact-once UTC-window routing and
   atomic publication for Capture v1. Legacy routing remains isolated in
   `windowing` during the staged rollout.
6. `host.recorder.capture_reader` validates and reads sealed Capture files.
7. `host.recorder.archive_v1` performs offline, deterministic compaction and
   `host.recorder.archive_reader` validates the published Archive product.
8. `host.host_recorder` is the application composition root and owns the
   protocol loop and operational lifecycle.

Dependencies point toward the model and ports. Infrastructure modules must not
import the recorder application entrypoint.

## Capture invariants

- Samples are identified by `(boot_epoch, sample_seq)`.
- A committed sample is not duplicated within one Capture file.
- An ingest batch is published only after its samples and timing anchors are
  flushed.
- Active output uses `.partial`; publication sets `complete=true`, flushes,
  closes, and atomically renames the file.
- Timing uncertainty is data, not a hard-coded system-wide constant.
- Physical sensor loss, logical sequence gaps, transport diagnostics, and time
  uncertainty are separate concepts.

## Archive boundary

Archive conversion operates only on sealed Capture files. It must be
idempotent, preserve measurement values exactly, account for every input sample
exactly once, record gaps and quality intervals, and publish a checksum-backed
manifest. Capture deletion is a separate retention decision and is never part
of conversion.

The first Archive implementation will be a single daily local process. Storage
backends and automatic deletion remain outside the conversion domain so a local
disk, mounted SSD, or remote storage can be selected later.

The offline command is:

```text
./hostctl archive --day YYYY-MM-DD --output DAY.archive.h5 CAPTURE...
```

It refuses incomplete, invalid, unresolved, wrong-day, mixed-sensor, and
duplicate sample inputs. Publication produces both an atomically renamed HDF5
file and a SHA-256 sidecar. It never deletes Capture inputs.

## Rollout gate

`storage.capture_schema` remains explicitly set to `5` in the live system
configuration until firmware v0.4 reports the complete configuration contract
and a real Capture v1 trial passes. Switching it to `1` is a deliberate rollout
operation, not an automatic side effect of installing host code. A Capture v1
worker refuses firmware that reports an unknown filter profile or configuration
revision, so partial upgrades fail closed.
