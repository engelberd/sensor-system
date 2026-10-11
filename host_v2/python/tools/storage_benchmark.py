#!/usr/bin/env python3
"""Bounded-memory benchmark for Sensor System Capture/Archive candidates.

The benchmark generates deterministic signed-24 XYZ blocks. Every writer and
validator processes one bounded block at a time; a result is rejected when the
decoded SHA-256 differs from the source stream.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import resource
import shutil
import struct
import subprocess
import sys
import tempfile
import time
from typing import BinaryIO, Iterator
import zlib

import numpy as np


MAGIC = b"SSARV1\0\0"
HEADER = struct.Struct("<8sQQ")
WAL_RECORD = struct.Struct("<4sIQQI")
FORMATS = (
    "wal",
    "packed",
    "gzip",
    "zstd",
    "gzip-shuffle",
    "zstd-shuffle",
    "hdf5-gzip",
)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--duration-s", type=float, default=600.0)
    parser.add_argument("--odr", type=float, default=125.0)
    parser.add_argument("--block-samples", type=int, default=16_384)
    parser.add_argument("--profile", choices=("quiet", "vibration", "noise"), default="vibration")
    parser.add_argument("--fsync-blocks", type=int, default=8)
    parser.add_argument("--formats", nargs="+", choices=FORMATS, default=list(FORMATS))
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--keep-files", action="store_true")
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--worker", action="store_true", help=argparse.SUPPRESS)
    parser.add_argument("--format", choices=FORMATS, help=argparse.SUPPRESS)
    return parser.parse_args()


def sample_blocks(total: int, block_samples: int, profile: str) -> Iterator[np.ndarray]:
    """Yield deterministic bounded int32 blocks whose values fit signed 24-bit."""
    offset = 0
    while offset < total:
        count = min(block_samples, total - offset)
        index = np.arange(offset, offset + count, dtype=np.float64)
        if profile == "quiet":
            noise_scale, a1, a2 = 6.0, 120.0, 30.0
        elif profile == "vibration":
            noise_scale, a1, a2 = 120.0, 18_000.0, 4_000.0
        else:
            noise_scale, a1, a2 = 20_000.0, 4_000.0, 1_000.0
        integer_index = np.arange(offset, offset + count, dtype=np.uint64)
        noise = np.empty((count, 3), dtype=np.float64)
        for axis in range(3):
            # SplitMix64 makes noise a pure function of (sequence, axis), so a
            # different benchmark block size cannot change the source stream.
            seed = (0x9E3779B97F4A7C15 * (axis + 1)) & 0xFFFF_FFFF_FFFF_FFFF
            mixed = integer_index + np.uint64(seed)
            mixed ^= mixed >> np.uint64(30)
            mixed *= np.uint64(0xBF58476D1CE4E5B9)
            mixed ^= mixed >> np.uint64(27)
            mixed *= np.uint64(0x94D049BB133111EB)
            mixed ^= mixed >> np.uint64(31)
            uniform = (mixed >> np.uint64(11)).astype(np.float64) / float(1 << 53)
            noise[:, axis] = (uniform * 2.0 - 1.0) * noise_scale
        values = np.empty((count, 3), dtype=np.float64)
        values[:, 0] = a1 * np.sin(index * 0.071) + a2 * np.sin(index * 0.013)
        values[:, 1] = a1 * np.sin(index * 0.047 + 0.7) + a2 * np.sin(index * 0.019)
        values[:, 2] = a1 * np.sin(index * 0.031 + 1.1) + a2 * np.sin(index * 0.023)
        values += noise
        yield np.rint(values).astype("<i4")
        offset += count


def pack_i24(samples: np.ndarray) -> bytes:
    unsigned = samples.astype("<u4", copy=False) & np.uint32(0x00FF_FFFF)
    packed = np.empty((samples.shape[0], 3, 3), dtype=np.uint8)
    packed[:, :, 0] = unsigned & 0xFF
    packed[:, :, 1] = (unsigned >> 8) & 0xFF
    packed[:, :, 2] = (unsigned >> 16) & 0xFF
    return packed.tobytes()


def shuffle_i32(samples: np.ndarray) -> bytes:
    elements = np.ascontiguousarray(samples, dtype="<i4").reshape(-1)
    return elements.view(np.uint8).reshape(-1, 4).T.tobytes()


def unshuffle_i32(encoded: bytes, sample_count: int) -> np.ndarray:
    element_count = sample_count * 3
    shuffled = np.frombuffer(encoded, dtype=np.uint8).reshape(4, element_count)
    native = np.ascontiguousarray(shuffled.T).reshape(sample_count, 3, 4)
    return native.reshape(sample_count, 12).view("<i4").reshape(sample_count, 3)


def source_digest(total: int, block_samples: int, profile: str) -> str:
    digest = hashlib.sha256()
    for samples in sample_blocks(total, block_samples, profile):
        digest.update(pack_i24(samples))
    return digest.hexdigest()


def write_header(stream: BinaryIO, total: int, block_samples: int) -> None:
    stream.write(HEADER.pack(MAGIC, total, block_samples))


def read_header(stream: BinaryIO, expected_total: int) -> None:
    raw = stream.read(HEADER.size)
    if len(raw) != HEADER.size:
        raise RuntimeError("truncated archive header")
    magic, total, _ = HEADER.unpack(raw)
    if magic != MAGIC or total != expected_total:
        raise RuntimeError("invalid archive header")


def write_packed(path: Path, total: int, block_samples: int, profile: str) -> None:
    with path.open("wb") as stream:
        write_header(stream, total, block_samples)
        for samples in sample_blocks(total, block_samples, profile):
            stream.write(pack_i24(samples))
        stream.flush()
        os.fsync(stream.fileno())


def write_wal(
    path: Path,
    total: int,
    block_samples: int,
    profile: str,
    fsync_blocks: int,
) -> None:
    if fsync_blocks <= 0:
        raise RuntimeError("fsync block interval must be positive")
    with path.open("wb") as stream:
        write_header(stream, total, block_samples)
        first = 0
        blocks_since_sync = 0
        for samples in sample_blocks(total, block_samples, profile):
            payload = pack_i24(samples)
            stream.write(
                WAL_RECORD.pack(
                    b"BLK1",
                    len(payload),
                    first,
                    samples.shape[0],
                    zlib.crc32(payload),
                )
            )
            stream.write(payload)
            first += samples.shape[0]
            blocks_since_sync += 1
            if blocks_since_sync == fsync_blocks:
                stream.flush()
                os.fsync(stream.fileno())
                blocks_since_sync = 0
        stream.flush()
        os.fsync(stream.fileno())


def write_gzip(
    path: Path,
    total: int,
    block_samples: int,
    profile: str,
    *,
    shuffled: bool = False,
) -> None:
    with path.open("wb") as raw:
        with gzip.GzipFile(fileobj=raw, mode="wb", compresslevel=6, mtime=0) as stream:
            write_header(stream, total, block_samples)
            for samples in sample_blocks(total, block_samples, profile):
                stream.write(shuffle_i32(samples) if shuffled else pack_i24(samples))
        raw.flush()
        os.fsync(raw.fileno())


def write_zstd(
    path: Path,
    total: int,
    block_samples: int,
    profile: str,
    *,
    shuffled: bool = False,
) -> None:
    executable = shutil.which("zstd")
    if executable is None:
        raise RuntimeError("zstd executable is unavailable")
    with path.open("wb") as output:
        process = subprocess.Popen(
            [executable, "-q", "-3", "-T1", "-c"],
            stdin=subprocess.PIPE,
            stdout=output,
        )
        assert process.stdin is not None
        try:
            write_header(process.stdin, total, block_samples)
            for samples in sample_blocks(total, block_samples, profile):
                process.stdin.write(shuffle_i32(samples) if shuffled else pack_i24(samples))
            process.stdin.close()
            return_code = process.wait()
        except BaseException:
            process.kill()
            process.wait()
            raise
        if return_code != 0:
            raise RuntimeError(f"zstd failed with exit code {return_code}")
        output.flush()
        os.fsync(output.fileno())


def write_hdf5(path: Path, total: int, block_samples: int, profile: str) -> None:
    import h5py  # type: ignore

    with h5py.File(path, "w") as handle:
        handle.attrs["format_name"] = "benchmark-archive"
        handle.attrs["sample_count"] = total
        dataset = handle.create_dataset(
            "raw_xyz",
            shape=(total, 3),
            dtype="<i4",
            chunks=(min(block_samples, max(1, total)), 3),
            compression="gzip",
            compression_opts=4,
            shuffle=True,
            fletcher32=True,
        )
        offset = 0
        for samples in sample_blocks(total, block_samples, profile):
            dataset[offset : offset + samples.shape[0]] = samples
            offset += samples.shape[0]
        handle.flush()
        file_handle = handle.id.get_vfd_handle()
        if isinstance(file_handle, int):
            os.fsync(file_handle)


def digest_stream(stream: BinaryIO, expected_total: int, block_bytes: int) -> str:
    read_header(stream, expected_total)
    digest = hashlib.sha256()
    byte_count = 0
    while True:
        block = stream.read(block_bytes)
        if not block:
            break
        digest.update(block)
        byte_count += len(block)
    expected_bytes = expected_total * 9
    if byte_count != expected_bytes:
        raise RuntimeError(f"decoded {byte_count} sample bytes, expected {expected_bytes}")
    return digest.hexdigest()


def read_exact(stream: BinaryIO, byte_count: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < byte_count:
        part = stream.read(byte_count - len(chunks))
        if not part:
            raise RuntimeError("truncated shuffled sample block")
        chunks.extend(part)
    return bytes(chunks)


def digest_shuffled_stream(
    stream: BinaryIO,
    expected_total: int,
    block_samples: int,
) -> str:
    read_header(stream, expected_total)
    digest = hashlib.sha256()
    offset = 0
    while offset < expected_total:
        count = min(block_samples, expected_total - offset)
        encoded = read_exact(stream, count * 12)
        digest.update(pack_i24(unshuffle_i32(encoded, count)))
        offset += count
    if stream.read(1):
        raise RuntimeError("unexpected bytes after shuffled sample stream")
    return digest.hexdigest()


def recover_wal(path: Path, expected_total: int) -> tuple[str, int, int]:
    digest = hashlib.sha256()
    cursor = 0
    valid_end = 0
    with path.open("rb") as stream:
        try:
            read_header(stream, expected_total)
        except RuntimeError:
            return digest.hexdigest(), cursor, valid_end
        valid_end = HEADER.size
        while True:
            raw_header = stream.read(WAL_RECORD.size)
            if not raw_header:
                break
            if len(raw_header) != WAL_RECORD.size:
                break
            magic, payload_len, first, sample_count, expected_crc = WAL_RECORD.unpack(raw_header)
            if magic != b"BLK1" or first != cursor or payload_len != sample_count * 9:
                break
            payload = stream.read(payload_len)
            if len(payload) != payload_len or zlib.crc32(payload) != expected_crc:
                break
            digest.update(payload)
            cursor += sample_count
            valid_end = stream.tell()
    return digest.hexdigest(), cursor, valid_end


def verify_file(
    kind: str,
    path: Path,
    total: int,
    block_samples: int,
) -> str:
    if kind == "wal":
        digest, cursor, valid_end = recover_wal(path, total)
        if cursor != total or valid_end != path.stat().st_size:
            raise RuntimeError("complete WAL did not recover to its full cursor")
        return digest
    if kind == "packed":
        with path.open("rb") as stream:
            return digest_stream(stream, total, block_samples * 9)
    if kind in ("gzip", "gzip-shuffle"):
        with gzip.open(path, "rb") as stream:
            if kind == "gzip-shuffle":
                return digest_shuffled_stream(stream, total, block_samples)
            return digest_stream(stream, total, block_samples * 9)
    if kind in ("zstd", "zstd-shuffle"):
        executable = shutil.which("zstd")
        if executable is None:
            raise RuntimeError("zstd executable is unavailable")
        process = subprocess.Popen(
            [executable, "-q", "-d", "-c", str(path)],
            stdout=subprocess.PIPE,
        )
        assert process.stdout is not None
        try:
            digest = (
                digest_shuffled_stream(process.stdout, total, block_samples)
                if kind == "zstd-shuffle"
                else digest_stream(process.stdout, total, block_samples * 9)
            )
            process.stdout.close()
            return_code = process.wait()
        except BaseException:
            process.kill()
            process.wait()
            raise
        if return_code != 0:
            raise RuntimeError(f"zstd validation failed with exit code {return_code}")
        return digest
    if kind == "hdf5-gzip":
        import h5py  # type: ignore

        digest = hashlib.sha256()
        with h5py.File(path, "r") as handle:
            dataset = handle["raw_xyz"]
            if dataset.shape != (total, 3):
                raise RuntimeError(f"unexpected HDF5 shape {dataset.shape}")
            for start in range(0, total, block_samples):
                digest.update(pack_i24(dataset[start : start + block_samples]))
        return digest.hexdigest()
    raise AssertionError(kind)


def peak_rss_bytes() -> int:
    own = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    children = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
    value = max(own, children)
    return int(value if sys.platform == "darwin" else value * 1024)


def worker(args: argparse.Namespace) -> dict[str, object]:
    if args.format is None or args.output_dir is None:
        raise SystemExit("worker requires --format and --output-dir")
    if args.duration_s <= 0 or args.odr <= 0 or args.block_samples <= 0:
        raise SystemExit("duration, ODR and block size must be positive")
    total = int(round(args.duration_s * args.odr))
    suffix = {
        "wal": ".wal",
        "packed": ".i24",
        "gzip": ".i24.gz",
        "zstd": ".i24.zst",
        "gzip-shuffle": ".i32s.gz",
        "zstd-shuffle": ".i32s.zst",
        "hdf5-gzip": ".h5",
    }[args.format]
    path = args.output_dir / f"{args.format}{suffix}"
    expected_digest = source_digest(total, args.block_samples, args.profile)

    writers = {
        "packed": write_packed,
        "gzip": write_gzip,
        "zstd": write_zstd,
        "gzip-shuffle": lambda *values: write_gzip(*values, shuffled=True),
        "zstd-shuffle": lambda *values: write_zstd(*values, shuffled=True),
        "hdf5-gzip": write_hdf5,
    }
    write_start = time.perf_counter()
    if args.format == "wal":
        write_wal(
            path,
            total,
            args.block_samples,
            args.profile,
            args.fsync_blocks,
        )
    else:
        writers[args.format](path, total, args.block_samples, args.profile)
    write_seconds = time.perf_counter() - write_start

    verify_start = time.perf_counter()
    actual_digest = verify_file(args.format, path, total, args.block_samples)
    verify_seconds = time.perf_counter() - verify_start
    if actual_digest != expected_digest:
        raise RuntimeError("decoded stream digest differs from source")

    raw_bytes = total * 9
    result = {
        "format": args.format,
        "samples": total,
        "raw_i24_bytes": raw_bytes,
        "file_bytes": path.stat().st_size,
        "bytes_per_sample": path.stat().st_size / total,
        "ratio_to_i24": path.stat().st_size / raw_bytes,
        "write_seconds": write_seconds,
        "verify_seconds": verify_seconds,
        "peak_rss_bytes": peak_rss_bytes(),
        "sha256": actual_digest,
    }
    if args.format == "wal" and path.stat().st_size > HEADER.size + 7:
        truncated = path.with_suffix(".truncated.wal")
        shutil.copyfile(path, truncated)
        with truncated.open("r+b") as stream:
            stream.truncate(path.stat().st_size - 7)
        recovery_start = time.perf_counter()
        _, recovered_cursor, valid_end = recover_wal(truncated, total)
        result["truncated_recovery_seconds"] = time.perf_counter() - recovery_start
        result["truncated_recovered_cursor"] = recovered_cursor
        result["truncated_valid_bytes"] = valid_end
        truncated.unlink()
    return result


def orchestrate(args: argparse.Namespace) -> list[dict[str, object]]:
    owned_temp = args.output_dir is None
    output = args.output_dir or Path(tempfile.mkdtemp(prefix="sensor-storage-benchmark-"))
    output.mkdir(parents=True, exist_ok=True)
    results: list[dict[str, object]] = []
    try:
        for kind in args.formats:
            command = [
                sys.executable,
                str(Path(__file__).resolve()),
                "--worker",
                "--format",
                kind,
                "--duration-s",
                str(args.duration_s),
                "--odr",
                str(args.odr),
                "--block-samples",
                str(args.block_samples),
                "--profile",
                args.profile,
                "--fsync-blocks",
                str(args.fsync_blocks),
                "--output-dir",
                str(output),
            ]
            completed = subprocess.run(command, check=True, text=True, capture_output=True)
            results.append(json.loads(completed.stdout))
    finally:
        if owned_temp and not args.keep_files:
            shutil.rmtree(output)
    return results


def print_table(results: list[dict[str, object]]) -> None:
    print("format       MiB      B/sample  ratio    write_s  verify_s  peak_RSS_MiB")
    for result in results:
        print(
            f"{result['format']:<12} "
            f"{int(result['file_bytes']) / 2**20:8.2f} "
            f"{float(result['bytes_per_sample']):9.3f} "
            f"{float(result['ratio_to_i24']):7.3f} "
            f"{float(result['write_seconds']):8.3f} "
            f"{float(result['verify_seconds']):9.3f} "
            f"{int(result['peak_rss_bytes']) / 2**20:12.2f}"
        )


def main() -> int:
    args = parse_args()
    if args.worker:
        print(json.dumps(worker(args), sort_keys=True))
        return 0
    results = orchestrate(args)
    if args.json:
        print(json.dumps(results, indent=2, sort_keys=True))
    else:
        print_table(results)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
