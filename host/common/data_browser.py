from __future__ import annotations

import mimetypes
import tempfile
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any
from urllib.parse import quote, unquote

from host.common.system_config import HostSystemConfig


PROJECT_ROOT = Path(__file__).resolve().parents[2]
MAX_DATA_ITEMS = 500
MAX_SEARCH_ITEMS = 1_000


@dataclass(frozen=True)
class FileDownload:
    path: Path
    media_type: str
    download_name: str
    cleanup_path: Path | None = None


class DataRepository:
    def __init__(self, config_path: str | Path) -> None:
        self.config_path = Path(config_path)

    def root_path(self) -> Path:
        config = HostSystemConfig.load(self.config_path)
        root = Path(config.storage.root_dir)
        if not root.is_absolute():
            root = PROJECT_ROOT / root
        return root.resolve()

    def _resolve(self, raw_relative: str | None) -> Path:
        root = self.root_path()
        target = root
        if raw_relative:
            target = (root / unquote(raw_relative)).resolve()
        if root != target and root not in target.parents:
            raise ValueError("path escapes data root")
        return target

    def list(self, raw_relative: str | None) -> dict[str, Any]:
        root = self.root_path()
        target = self._resolve(raw_relative)
        if not root.exists():
            return {
                "root": str(root),
                "relative_path": ".",
                "exists": False,
                "items": [],
            }
        if not target.exists():
            raise FileNotFoundError("requested data path does not exist")
        if not target.is_dir():
            raise NotADirectoryError("requested data path is not a directory")

        items = [
            self._item_payload(root, child)
            for child in sorted(target.iterdir(), key=lambda entry: (not entry.is_dir(), entry.name.lower()))[:MAX_DATA_ITEMS]
        ]
        relative_path = "." if target == root else target.relative_to(root).as_posix()
        parent = None
        if target != root:
            parent_path = target.parent.relative_to(root)
            parent = "." if str(parent_path) == "." else parent_path.as_posix()
        return {
            "root": str(root),
            "relative_path": relative_path,
            "parent_relative_path": parent,
            "exists": True,
            "items": items,
        }

    def search(self, raw_query: str | None) -> dict[str, Any]:
        root = self.root_path()
        query = (raw_query or "").strip()
        tokens = [token.casefold() for token in query.split() if token.strip()]
        if not root.exists():
            return {"root": str(root), "query": query, "exists": False, "items": []}
        if not query:
            return {"root": str(root), "query": query, "exists": True, "items": []}

        items: list[dict[str, Any]] = []
        for child in sorted(root.rglob("*"), key=lambda entry: (not entry.is_dir(), str(entry).lower())):
            if len(items) >= MAX_SEARCH_ITEMS:
                break
            relative = child.relative_to(root).as_posix()
            haystack = relative.casefold()
            if all(token in haystack for token in tokens):
                items.append(self._item_payload(root, child))

        return {
            "root": str(root),
            "query": query,
            "exists": True,
            "items": items,
            "truncated": len(items) >= MAX_SEARCH_ITEMS,
        }

    def download(self, raw_relative: str | None) -> FileDownload:
        if not raw_relative:
            raise ValueError("missing data file path")
        target = self._resolve(raw_relative)
        if not target.exists():
            raise FileNotFoundError("requested data file does not exist")
        if target.is_dir():
            return self._archive_targets([target], archive_stem=target.name)
        media_type = mimetypes.guess_type(target.name)[0] or "application/octet-stream"
        return FileDownload(path=target, media_type=media_type, download_name=target.name)

    def download_bundle(self, raw_paths: list[str]) -> FileDownload:
        if not raw_paths:
            raise ValueError("missing selected data paths")

        targets: list[Path] = []
        seen: set[Path] = set()
        for raw_path in raw_paths:
            target = self._resolve(raw_path)
            if not target.exists():
                raise FileNotFoundError(f"selected data path does not exist: {raw_path}")
            if target in seen:
                continue
            seen.add(target)
            targets.append(target)

        if len(targets) == 1 and targets[0].is_file():
            target = targets[0]
            media_type = mimetypes.guess_type(target.name)[0] or "application/octet-stream"
            return FileDownload(path=target, media_type=media_type, download_name=target.name)

        archive_stem = targets[0].name if len(targets) == 1 else "data-selection"
        return self._archive_targets(targets, archive_stem=archive_stem)

    def _item_payload(self, root: Path, child: Path) -> dict[str, Any]:
        stat = child.stat()
        relative = child.relative_to(root).as_posix()
        return {
            "name": child.name,
            "relative_path": relative,
            "type": "directory" if child.is_dir() else "file",
            "size_bytes": stat.st_size,
            "modified_utc": datetime.fromtimestamp(stat.st_mtime, tz=timezone.utc).isoformat(),
            "download_url": f"/api/data/download?path={quote(relative)}",
        }

    def _archive_targets(self, targets: list[Path], *, archive_stem: str) -> FileDownload:
        root = self.root_path()
        temp_handle = tempfile.NamedTemporaryFile(prefix=f"{archive_stem}_", suffix=".zip", delete=False)
        temp_handle.close()
        archive_path = Path(temp_handle.name)
        try:
            with zipfile.ZipFile(archive_path, "w", compression=zipfile.ZIP_DEFLATED) as archive:
                archived_names: set[str] = set()
                for target in targets:
                    if target.is_dir():
                        for child in sorted(target.rglob("*")):
                            if child.is_dir():
                                continue
                            arcname = child.relative_to(root).as_posix()
                            if arcname in archived_names:
                                continue
                            archive.write(child, arcname=arcname)
                            archived_names.add(arcname)
                    else:
                        arcname = target.relative_to(root).as_posix()
                        if arcname in archived_names:
                            continue
                        archive.write(target, arcname=arcname)
                        archived_names.add(arcname)
        except Exception:
            archive_path.unlink(missing_ok=True)
            raise
        return FileDownload(
            path=archive_path,
            media_type="application/zip",
            download_name=f"{archive_stem}.zip",
            cleanup_path=archive_path,
        )
