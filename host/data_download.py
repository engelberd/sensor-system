from __future__ import annotations

import argparse
import functools
import html
import io
import os
from http import HTTPStatus
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from host.common.system_config import HostSystemConfig


PROJECT_ROOT = Path(__file__).resolve().parent.parent


def resolve_root(config_path: str | Path, root_dir_override: str | None = None) -> Path:
    if root_dir_override:
        root = Path(root_dir_override)
    else:
        config = HostSystemConfig.load(config_path)
        root = Path(config.storage.root_dir)
    if not root.is_absolute():
        root = PROJECT_ROOT / root
    return root.resolve()


class DownloadOnlyHandler(SimpleHTTPRequestHandler):
    server_version = "SensorSystemDataDownload/0.1"

    def __init__(self, *args, directory: str | None = None, **kwargs) -> None:
        self._content_disposition: str | None = None
        super().__init__(*args, directory=directory, **kwargs)

    def end_headers(self) -> None:
        if self._content_disposition:
            self.send_header("Content-Disposition", self._content_disposition)
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self) -> None:  # noqa: N802
        self.send_error(HTTPStatus.METHOD_NOT_ALLOWED, "upload is not supported")

    def do_PUT(self) -> None:  # noqa: N802
        self.send_error(HTTPStatus.METHOD_NOT_ALLOWED, "upload is not supported")

    def do_DELETE(self) -> None:  # noqa: N802
        self.send_error(HTTPStatus.METHOD_NOT_ALLOWED, "delete is not supported")

    def do_PATCH(self) -> None:  # noqa: N802
        self.send_error(HTTPStatus.METHOD_NOT_ALLOWED, "patch is not supported")

    def list_directory(self, path: str) -> io.BytesIO | None:
        try:
            entries = sorted(os.scandir(path), key=lambda entry: (not entry.is_dir(), entry.name.lower()))
        except OSError:
            self.send_error(HTTPStatus.NOT_FOUND, "cannot list directory")
            return None

        display_path = "/" + Path(path).resolve().relative_to(Path(self.directory).resolve()).as_posix().lstrip(".")
        if display_path == "/.":
            display_path = "/"

        lines = [
            "<!DOCTYPE html>",
            "<html lang=\"pl\">",
            "<head>",
            "<meta charset=\"utf-8\">",
            f"<title>Pliki: {html.escape(display_path)}</title>",
            "<style>",
            "body{font-family:sans-serif;max-width:960px;margin:2rem auto;padding:0 1rem;line-height:1.5;}",
            "table{width:100%;border-collapse:collapse;}",
            "th,td{padding:.55rem;border-bottom:1px solid #ddd;text-align:left;}",
            "a{text-decoration:none;}",
            "</style>",
            "</head>",
            "<body>",
            f"<h1>Udostepnione pliki: {html.escape(display_path)}</h1>",
            "<p>Serwer jest tylko do odczytu. Kliknij nazwe pliku, aby pobrac.</p>",
            "<table>",
            "<tr><th>Nazwa</th><th>Typ</th><th>Rozmiar [B]</th></tr>",
        ]

        relative_path = Path(path).resolve().relative_to(Path(self.directory).resolve())
        if str(relative_path) != ".":
            parent_href = "/" if relative_path.parent == Path(".") else f"/{relative_path.parent.as_posix()}/"
            lines.append(f"<tr><td><a href=\"{html.escape(parent_href)}\">..</a></td><td>katalog</td><td></td></tr>")

        for entry in entries:
            name = entry.name + ("/" if entry.is_dir() else "")
            href = name
            size = "" if entry.is_dir() else str(entry.stat().st_size)
            kind = "katalog" if entry.is_dir() else "plik"
            lines.append(
                "<tr>"
                f"<td><a href=\"{html.escape(href)}\">{html.escape(name)}</a></td>"
                f"<td>{kind}</td>"
                f"<td>{size}</td>"
                "</tr>"
            )

        lines.extend(["</table>", "</body>", "</html>"])
        encoded = "\n".join(lines).encode("utf-8", "surrogateescape")
        fileobj = io.BytesIO(encoded)
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(encoded)))
        self.end_headers()
        return fileobj

    def send_head(self):  # type: ignore[override]
        self._content_disposition = None
        path = Path(self.translate_path(self.path))
        if path.is_file():
            self._content_disposition = f'attachment; filename="{path.name}"'
        response = super().send_head()
        return response


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Serve the configured storage root as download-only files")
    parser.add_argument("--config", default=str(PROJECT_ROOT / "host" / "system_config.json"))
    parser.add_argument("--root-dir", help="Override the storage root from config")
    parser.add_argument("--host", default="0.0.0.0")
    parser.add_argument("--port", type=int, default=8091)
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    root = resolve_root(args.config, args.root_dir)
    root.mkdir(parents=True, exist_ok=True)

    handler = functools.partial(DownloadOnlyHandler, directory=str(root))
    server = ThreadingHTTPServer((args.host, args.port), handler)
    print(f"Serving {root} on http://{args.host}:{args.port}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
