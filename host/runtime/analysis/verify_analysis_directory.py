#!/usr/bin/env python3
"""Verify every published HDF5 window in an Analysis directory."""
import argparse
from pathlib import Path

import numpy as np

from load_analysis_window import load_window


def verify(root):
    count = samples = gaps = breaks = 0
    for path in sorted(root.rglob("*.h5")):
        window = load_window(path)
        meta = window["metadata"]
        if window["samples"].dtype != np.dtype("int32") or meta.get("axis_unit") != "raw":
            raise ValueError(f"{path}: samples must contain raw int32 axes")
        if not meta.get("configuration"):
            raise ValueError(f"{path}: missing complete configuration")
        count += 1
        samples += len(window["samples"])
        gaps += window["missing_samples"]
        breaks += window["continuity_breaks"]
    if count == 0:
        raise ValueError("no HDF5 products to verify")
    print(f"verified: files={count} samples={samples} missing={gaps} breaks={breaks}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path)
    verify(parser.parse_args().root)
