#!/usr/bin/env python3
"""生成可签名的桌宠动作素材包和清单。"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import zipfile


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--version", required=True, help="SemVer，例如 2026.8.1")
    parser.add_argument("--base-url", required=True, help="发布附件所在的 HTTPS 地址")
    parser.add_argument("--min-app-version", default="0.2.2")
    parser.add_argument("--output", default="release-assets")
    args = parser.parse_args()

    project = Path(__file__).resolve().parents[1]
    output = (project / args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    pack_name = f"character-assets-{args.version}.zip"
    pack_path = output / pack_name

    files: list[tuple[Path, str]] = []
    for role in ("一二", "布布"):
        role_dir = project / "assets" / "characters" / role
        for path in sorted(role_dir.glob("*.gif"), key=lambda item: item.name):
            files.append((path, f"{role}/{path.name}"))
    rules = project / "assets" / "action-rules.json"
    if rules.exists():
        files.append((rules, "rules.json"))

    with zipfile.ZipFile(pack_path, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
        for source, archive_name in files:
            info = zipfile.ZipInfo(archive_name, date_time=(2026, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o644 << 16
            archive.writestr(info, source.read_bytes(), compresslevel=9)

    digest = hashlib.sha256(pack_path.read_bytes()).hexdigest()
    base_url = args.base_url.rstrip("/")
    manifest = {
        "schemaVersion": 1,
        "version": args.version,
        "minAppVersion": args.min_app_version,
        "packUrl": f"{base_url}/{pack_name}",
        "sha256": digest,
        "signature": "",
    }
    unsigned = output / "asset-manifest.unsigned.json"
    unsigned.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(pack_path)
    print(unsigned)


if __name__ == "__main__":
    main()
