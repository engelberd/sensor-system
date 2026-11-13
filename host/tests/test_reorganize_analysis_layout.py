import importlib.util
from pathlib import Path
import tempfile
import unittest

import h5py


SCRIPT = (
    Path(__file__).resolve().parents[1]
    / "runtime"
    / "analysis"
    / "reorganize_analysis_layout.py"
)
SPEC = importlib.util.spec_from_file_location("reorganize_analysis_layout", SCRIPT)
LAYOUT = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(LAYOUT)


class ReorganizeAnalysisLayoutTests(unittest.TestCase):
    def test_moves_flat_product_by_metadata_without_changing_name(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "line-b_2026-11-13_10-20.h5"
            with h5py.File(source, "w") as file:
                file.attrs["channel_name"] = "line-b"
                file.attrs["first_sample_time_local"] = "2026-11-13T10:20:00+01:00"

            moves = LAYOUT.plan_moves(root)
            destination = root / "2026-11-13" / "line-b" / source.name
            self.assertEqual(moves, [(source, destination)])
            LAYOUT.apply_moves(root, moves)
            self.assertFalse(source.exists())
            self.assertTrue(destination.is_file())

    def test_refuses_to_replace_existing_product(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "line-a_2026-11-13_10-20.h5"
            with h5py.File(source, "w") as file:
                file.attrs["channel_name"] = "line-a"
                file.attrs["first_sample_time_local"] = "2026-11-13T10:20:00+01:00"
            destination = root / "2026-11-13" / "line-a" / source.name
            destination.parent.mkdir(parents=True)
            destination.write_bytes(b"existing")

            with self.assertRaises(FileExistsError):
                LAYOUT.apply_moves(root, LAYOUT.plan_moves(root))
            self.assertTrue(source.is_file())
            self.assertEqual(destination.read_bytes(), b"existing")


if __name__ == "__main__":
    unittest.main()
