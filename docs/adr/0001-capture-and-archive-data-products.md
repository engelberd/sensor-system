# ADR 0001: Separate Capture and Archive data products

- Status: accepted
- Date: 2026-08-02

## Context

The recorder must survive interrupted writes, retain enough evidence to diagnose
timing and transport problems, and produce files quickly while acquisition is
running. Long-term storage has different requirements: small size, stable
semantics, readability, and only measurement-relevant quality information.

One format optimized for both purposes either repeats diagnostics for years or
removes evidence before a capture has been validated. Derived, resampled data
cannot replace the original measurement because it changes sample values and
time support.

## Decision

The system has three explicit data products:

1. **Capture HDF5 v1** is a crash-safe technical journal retained for 30 days.
2. **Archive HDF5 v1** is a compact, validated source record retained long-term.
3. **Derived data** contains resampled or cross-sensor-aligned results and is
   reproducible from Archive data.

Capture and Archive files contain one physical sensor. Files are partitioned by
UTC time for storage, while continuity is defined by stream segments and sample
sequence, never by filenames.

Firmware output samples are stored losslessly as signed values in an `int32`
container. Conversion to `m/s^2` is performed by readers using recorded range,
scale, processing, and calibration metadata.

Capture conversion, Archive validation, replication, and Capture deletion are
separate idempotent operations. No automatic deletion is allowed until an
Archive file is validated and a future storage policy confirms replication.

## Consequences

- Capture may change more frequently than Archive, but both formats are
  independently versioned.
- The recorder remains independent of long-term storage and network transfer.
- Archive files retain gaps, timing uncertainty, configuration, calibration,
  and measurement-quality intervals, but not packet-level diagnostics.
- A daily compactor and validator are required.
- During the prototype phase, old schema compatibility is not required. Once
  Archive v1 is declared production, incompatible changes require a new major
  schema version.

The operational policy, validation gate, locking rules, and concrete Archive v1
layout are specified in [HDF5-DATA-POLICY.md](../HDF5-DATA-POLICY.md).

## Rejected alternatives

- **Keep only Capture forever:** simple but retains transport repetition and
  couples analysis to recorder internals.
- **Write Archive directly:** loses crash-recovery and diagnostic evidence and
  makes acquisition depend on compaction logic.
- **Store only resampled data:** irreversibly changes the source measurement.
- **One HDF5 file for all sensors:** increases corruption blast radius and
  complicates independent sensor replacement and retention.
- **Store UTC for every sample:** wastes space and incorrectly implies that all
  timestamps have equal certainty.
