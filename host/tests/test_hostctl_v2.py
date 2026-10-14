from __future__ import annotations

import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
HOSTCTL = ROOT / "hostctl"


class HostctlV2Tests(unittest.TestCase):
    def run_hostctl(self, *arguments: str, env: dict[str, str] | None = None):
        process_env = os.environ.copy()
        if env:
            process_env.update(env)
        return subprocess.run(
            [str(HOSTCTL), *arguments],
            cwd="/",
            env=process_env,
            text=True,
            capture_output=True,
            check=False,
        )

    def fake_binary(self, directory: Path) -> tuple[Path, Path]:
        output = directory / "arguments.txt"
        binary = directory / "fake-binary"
        binary.write_text(
            "#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >\"$SENSOR_TEST_ARGUMENTS\"\n",
            encoding="utf-8",
        )
        binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
        return binary, output

    def test_help_exposes_only_v2_product_commands(self):
        result = self.run_hostctl("--help")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("capture validate", result.stdout)
        self.assertIn("node <sensor-hil command>", result.stdout)
        self.assertNotIn("Run production recorder", result.stdout)
        self.assertNotIn("Run configurator", result.stdout)

    def test_retired_legacy_command_fails_with_migration_pointer(self):
        result = self.run_hostctl("recorder")
        self.assertEqual(result.returncode, 64)
        self.assertIn("retired Python host", result.stderr)
        self.assertIn("docs/legacy/", result.stderr)

    def test_validate_uses_v2_defaults_from_any_working_directory(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary, output = self.fake_binary(Path(temporary))
            result = self.run_hostctl(
                "validate",
                env={
                    "SENSOR_HOST_BINARY": str(binary),
                    "SENSOR_TEST_ARGUMENTS": str(output),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                output.read_text(encoding="utf-8").splitlines(),
                [
                    "validate",
                    "--config",
                    str(ROOT / "host/system_config.json"),
                    "--repository-root",
                    str(ROOT),
                ],
            )

    def test_explicit_config_replaces_the_default_instead_of_repeating_it(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary, output = self.fake_binary(Path(temporary))
            result = self.run_hostctl(
                "paths",
                "--config",
                "/tmp/profile.json",
                "--init",
                env={
                    "SENSOR_HOST_BINARY": str(binary),
                    "SENSOR_TEST_ARGUMENTS": str(output),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            arguments = output.read_text(encoding="utf-8").splitlines()
            self.assertEqual(arguments.count("--config"), 1)
            self.assertEqual(
                arguments,
                [
                    "paths",
                    "--repository-root",
                    str(ROOT),
                    "--config",
                    "/tmp/profile.json",
                    "--init",
                ],
            )

    def test_node_routes_directly_to_native_protocol_5_tool(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary, output = self.fake_binary(Path(temporary))
            result = self.run_hostctl(
                "node",
                "identify",
                "/dev/line-a",
                "1",
                "115200",
                env={
                    "SENSOR_HOST_HIL_BINARY": str(binary),
                    "SENSOR_TEST_ARGUMENTS": str(output),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                output.read_text(encoding="utf-8").splitlines(),
                ["identify", "/dev/line-a", "1", "115200"],
            )

    def test_channel_routes_to_local_operations_ipc_client(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary, output = self.fake_binary(Path(temporary))
            result = self.run_hostctl(
                "channel",
                "maintenance",
                "--channel",
                "3",
                "--operation-id",
                "99",
                env={
                    "SENSOR_OPERATIONS_BINARY": str(binary),
                    "SENSOR_HOST_OPERATIONS_SOCKET": "/tmp/operations.sock",
                    "SENSOR_TEST_ARGUMENTS": str(output),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                output.read_text(encoding="utf-8").splitlines(),
                [
                    "maintenance",
                    "--channel",
                    "3",
                    "--operation-id",
                    "99",
                    "--socket",
                    "/tmp/operations.sock",
                ],
            )

    def test_update_composes_protocol_5_prepare_upload_and_exact_confirm(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            transcript = directory / "transcript.txt"
            image = directory / "package.json"
            image.write_text(
                '{"format":"sensor_system_node_update_package",'
                '"schema_version":1,"version":1287,"board_revision":2,'
                '"slot_a":{"path":"a.bin"},"slot_b":{"path":"b.bin"}}',
                encoding="utf-8",
            )
            hil = directory / "sensor-hil"
            hil.write_text(
                "#!/usr/bin/env bash\nprintf 'hil %s\\n' \"$*\" >>\"$SENSOR_TEST_ARGUMENTS\"\n",
                encoding="utf-8",
            )
            python = directory / "python"
            python.write_text(
                "#!/usr/bin/env bash\nprintf 'uploader %s\\n' \"$*\" >>\"$SENSOR_TEST_ARGUMENTS\"\n",
                encoding="utf-8",
            )
            hil.chmod(hil.stat().st_mode | stat.S_IXUSR)
            python.chmod(python.stat().st_mode | stat.S_IXUSR)
            result = self.run_hostctl(
                "update",
                "--port",
                "/dev/line-b",
                "--node",
                "1",
                "--hardware-id",
                "2f13349450fa4084",
                "--board-revision",
                "2",
                "--expected-firmware",
                "0.5.7",
                "--image",
                str(image),
                env={
                    "SENSOR_HOST_HIL_BINARY": str(hil),
                    "SENSOR_HOST_BOOTLOADER_PYTHON": str(python),
                    "SENSOR_HOST_PACKAGE_PYTHON": sys.executable,
                    "SENSOR_HOST_SYSTEMCTL": "sensor-system-test-no-systemctl",
                    "SENSOR_TEST_ARGUMENTS": str(transcript),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            calls = transcript.read_text(encoding="utf-8")
            self.assertIn(
                "hil prepare-update /dev/line-b 1 2f13349450fa4084 2 115200",
                calls,
            )
            self.assertIn("--enter none", calls)
            self.assertNotIn("--enter auto", calls)
            self.assertIn(
                "hil confirm-trial /dev/line-b 1 2f13349450fa4084 2 0.5.7 115200",
                calls,
            )


if __name__ == "__main__":
    unittest.main()
