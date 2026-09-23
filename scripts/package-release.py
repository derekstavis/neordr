#!/usr/bin/env python3
"""Validate a release tag and assemble a portable binary/plugin distribution."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parent.parent
TARGETS = (
    "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "aarch64-apple-darwin",
)


def validate_tag(tag):
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", tag) or tag != f"v{version}":
        raise SystemExit(f"Tag {tag!r} must match Cargo.toml version v{version}")
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--target", choices=TARGETS)
    parser.add_argument("--output", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    version = validate_tag(args.tag)
    if not args.target:
        print(version)
        return
    binary = ROOT / "target" / args.target / "release" / "neordr"
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    name = f"neordr-{args.tag}-{args.target}"
    with tempfile.TemporaryDirectory(prefix="neordr-package-") as temporary:
        package = Path(temporary) / name
        (package / "bin").mkdir(parents=True)
        shutil.copy2(binary, package / "bin/neordr")
        (package / "bin/neordr").chmod(0o755)
        shutil.copytree(ROOT / "lua", package / "share/neordr/lua")
        shutil.copytree(ROOT / "docs", package / "docs")
        shutil.copy2(ROOT / "README.md", package / "README.md")
        shutil.copy2(ROOT / "Cargo.lock", package / "Cargo.lock")
        for filename in ("CONTRIBUTING.md", "build_support/README.md", "site/assets/neordr-wordmark.svg"):
            source = ROOT / filename
            if source.exists():
                (package / filename).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, package / filename)
        if (ROOT / "LICENSE").exists():
            shutil.copy2(ROOT / "LICENSE", package / "LICENSE")
        shutil.copy2(ROOT / "THIRD_PARTY_NOTICES.md", package / "THIRD_PARTY_NOTICES.md")
        (package / "licenses").mkdir()
        shutil.copy2(ROOT / "vendor/herdr/LICENSE", package / "licenses/Herdr-Apache-2.0.txt")
        shutil.copy2(ROOT / "licenses/README.md", package / "licenses/README.md")
        spec = importlib.util.spec_from_file_location("collect_licenses", ROOT / "scripts/collect-licenses.py")
        collector = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(collector)
        collector.collect(args.target, package / "licenses")
        if (ROOT / "NOTICE").exists():
            shutil.copy2(ROOT / "NOTICE", package / "NOTICE")
        metadata = {
            "name": "neordr", "version": version, "target": args.target,
            "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "herdr_contract_commit": subprocess.check_output(["git", "-C", "vendor/herdr", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
        }
        (package / "BUILD.json").write_text(json.dumps(metadata, indent=2) + "\n")
        archive = output / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as compressed:
            compressed.add(package, arcname=name)
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_name(archive.name + ".sha256").write_text(f"{checksum}  {archive.name}\n")
    print(archive)


if __name__ == "__main__":
    main()
