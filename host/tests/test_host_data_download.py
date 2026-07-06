from __future__ import annotations

import http.client
import json
import tempfile
import threading
import unittest
from pathlib import Path

from host.data_download import DownloadOnlyHandler, resolve_root
from http.server import ThreadingHTTPServer


class DataDownloadTests(unittest.TestCase):
    def test_resolve_root_uses_storage_root_from_config(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            runs_root = temp_dir / "runs"
            config_path = temp_dir / "system_config.json"
            config_path.write_text(
                json.dumps(
                    {
                        "storage": {
                            "root_dir": str(runs_root),
                        },
                        "channels": [
                            {
                                "name": "line-a",
                                "label": "Linia A",
                                "port": "/dev/ttyUSB0",
                                "nodes": [1],
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            self.assertEqual(resolve_root(config_path), runs_root.resolve())

    def test_serves_files_as_attachments(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            root = temp_dir / "data"
            root.mkdir()
            file_path = root / "capture.h5"
            file_path.write_bytes(b"abc123")

            handler = lambda *args, **kwargs: DownloadOnlyHandler(*args, directory=str(root), **kwargs)
            server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=5)
                conn.request("GET", "/capture.h5")
                response = conn.getresponse()
                body = response.read()
                conn.close()
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)

            self.assertEqual(response.status, 200)
            self.assertEqual(body, b"abc123")
            self.assertEqual(response.getheader("Content-Disposition"), 'attachment; filename="capture.h5"')

    def test_rejects_write_method(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            temp_dir = Path(tmp)
            root = temp_dir / "data"
            root.mkdir()

            handler = lambda *args, **kwargs: DownloadOnlyHandler(*args, directory=str(root), **kwargs)
            server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                conn = http.client.HTTPConnection("127.0.0.1", server.server_port, timeout=5)
                conn.request("POST", "/")
                response = conn.getresponse()
                response.read()
                conn.close()
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)

            self.assertEqual(response.status, 405)


if __name__ == "__main__":
    unittest.main()
