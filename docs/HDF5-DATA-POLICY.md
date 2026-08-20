# HDF5 sample, file, validation and archive policy

## Product boundaries

The system uses three different products. They must not be silently converted
into one another:

1. **Capture HDF5 v1** is the live, crash-recoverable evidence journal.
2. **Archive HDF5 v1** is the immutable, validated long-term source record.
3. **Derived HDF5** is an analysis product. A uniform/resampled time grid belongs
   here and never replaces Capture or Archive samples.

Every Capture and Archive file contains exactly one physical sensor. Sample
identity is `(boot_epoch, sample_seq)`. A retransmission of that identity is not
a new sample.

## Sample policy

- Store every accepted firmware output sample once, losslessly, as signed 24-bit
  sensor values in an `int32` container. The current firmware output is already
  filtered and decimated; its nominal output ODR is sensor ODR divided by the
  recorded decimation factor.
- Never invent samples across sequence gaps, sensor loss, reboot, invalid timing,
  or an unresolved UTC assignment.
- Preserve raw values as the source of truth. Convert to `m/s^2` in readers using
  the configuration interval that covers each stream segment.
- Record configuration changes, gaps, timing uncertainty, and quality separately
  from sample values.
- Use measured timing to estimate effective ODR. Do not derive time from packet
  receive timestamps or from the filename.

## Capture file split and publication

- Default window: **600 seconds**, aligned to half-open UTC intervals
  `[start, end)`. This is the production default in `host/configs/common.json`.
- Route by acquisition UTC, not host receive time. A sample whose timing interval
  overlaps a boundary receives `BOUNDARY_UNCERTAIN` but is stored only once.
- Samples without a defensible UTC assignment go to `unresolved/`; they are not
  eligible for automatic daily archiving.
- Write only to `*.capture.h5.partial`. A completed window is flushed, fsynced,
  marked `complete=true`, closed, and atomically renamed to `*.capture.h5`.
- A graceful stop leaves the current UTC window partial so the next recorder can
  resume it. Startup truncates only an uncommitted raw tail and publishes stale,
  already-ended windows.

The 10-minute split limits the corruption/recovery blast radius and keeps
verification bounded without making daily archives depend on hundreds of tiny
files. Change it only as a product-wide setting after testing restart recovery,
dashboard discovery, and archive compaction.

## Locking and active-file rules

- Exactly one recorder may own a channel runtime namespace. The recorder holds
  `<runtime>/<channel>.recorder.lock` for its full lifetime and writes PID,
  channel, serial port, and destination into the lock file.
- An HDF5 library lock is not the ownership protocol. Disabling HDF5 locking does
  not make two writers safe.
- Normal readers, SFTP exports, validators, and the archive compactor consume only
  atomically published `*.capture.h5` or `*.archive.h5` files.
- Inspect a `.partial` file only after its recorder is stopped and only with the
  explicit `--allow-partial` verifier option.
- After a crash, do not delete a lock file to recover. Kernel advisory locks are
  released with the process; a retained text file is useful owner history.

At startup every partial is recovered independently:

- a valid incomplete file is resumed;
- a valid file already marked complete is atomically published;
- an unreadable, truncated, invalid, conflicting, or duplicate partial is moved
  to `quarantine/partial-recovery/YYYY-MM-DD/` with a `.recovery.json` sidecar;
- `partial_window_unrecoverable` is written to both runtime and durable channel
  diagnostics, and acquisition continues with a new window.

`unresolved/` is reserved for structurally valid samples without defensible UTC.
It is not a corruption quarantine.

The recorder checks free space before opening the serial port and every 30
seconds while running. The production reserve is 1 GiB (`min_free_bytes`). A
breach or HDF5/filesystem write failure stops the recorder with exit code 3. The
supervisor reports `failed-storage` and does not restart-loop; after repairing
storage, an operator explicitly starts or restarts the channel.

## Full-file validation gate

Run:

```bash
./hostctl verify --json path/to/file.capture.h5
./hostctl verify --json path/to/file.archive.h5
```

Validation reads every measurement chunk, which also exercises HDF5 Fletcher32
checksums, then verifies structural coverage, sample identity ordering, signed-24
range, configuration coverage, timing order, gaps, quality coverage, completion,
and (for Archive) the external SHA-256 manifest. The diagnostic report includes:

- sample, block/segment, gap, quality, and source counts;
- fraction with usable time or degraded quality;
- nominal output ODR, robust observed ODR, min/max interval ODR, and shift in ppm;
- whether a derived resampling layer is recommended.

An archive may be published only when all source Captures and the completed
Archive pass this gate. A warning about unavailable ODR is not permission to
resample; it means timing evidence is insufficient.

## Effective ODR and resampling

Estimate effective ODR only inside a continuous `(boot_epoch,
timing_segment_id, sample_seq)` run. Capture uses device-time block endpoints;
Archive uses its simplified UTC control points. Never fit across a gap or reboot.

For spectral analysis or cross-sensor alignment, create a separate Derived file:

1. reconstruct piecewise-affine sample time from Archive control points;
2. select an explicit target ODR (normally the recorded nominal output ODR);
3. apply an anti-alias filter when reducing rate;
4. resample each continuous, valid-quality run independently;
5. keep gaps as gaps and emit a validity/quality mask;
6. record source Archive `file_id`, SHA-256, algorithm/version, target ODR,
   filter, and parameters.

The verifier recommends this derived step when the absolute measured shift is
above 50 ppm. This threshold is an analysis default, not an acquisition alarm;
the default warning threshold is 1000 ppm and can be overridden at the CLI.

## Archive schedule and retention

- Build one Archive file per sensor and UTC day, no earlier than **00:40 UTC** on
  the following day. This leaves more than three 10-minute windows for late
  routing and finalization.
- Archive name: `sensor-<label>_YYYY-MM-DD.archive.h5`.
- Compact only complete UTC-assigned Capture files whose window starts on the
  requested UTC day. Reject duplicates and identity/configuration conflicts.
- Publish the HDF5 atomically, then publish `<file>.sha256` atomically.
- Keep Capture for at least 30 days. Deletion is a separate idempotent operation
  allowed only after Archive validation and confirmed replication. The current
  software intentionally performs no automatic measurement deletion.

## Archive HDF5 v1 contract

Root attributes identify schema, sensor, UTC day, completeness, compactor, and
summary counts. The datasets are:

| Path | Purpose |
| --- | --- |
| `/measurement/raw_xyz` | Bit-exact `(N,3)` signed raw samples |
| `/stream/segments` | Sample identity, timing run, and configuration coverage |
| `/stream/gaps` | Known/unknown loss boundaries without synthetic samples |
| `/timing/control_points` | Sparse device/UTC mapping and uncertainty |
| `/quality/intervals` | Run-length encoded per-sample quality flags |
| `/configuration/intervals` | ODR, range, offsets, decimation, and filter metadata |
| `/environment/temperature` | Sparse temperature observations with sample anchors |
| `/provenance/sources` | Capture file IDs, names, hashes, and contributed counts |

Archive v1 is source data, not a regular-grid matrix. Incompatible semantic or
layout changes require Archive schema v2.
