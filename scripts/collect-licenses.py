#!/usr/bin/env python3
"""Collect original Cargo crate license files and a per-target dependency manifest."""
import json
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parent.parent
OBJC2 = {
    ("block2", "0.6.2"): "b4167b582b2f75f9a1be75495c41b765344fd03c",
    ("dispatch2", "0.3.1"): "8852b424193ca41602281b3d7540d7c8ed51e49a",
    ("objc2", "0.6.4"): "8852b424193ca41602281b3d7540d7c8ed51e49a",
    ("objc2-encode", "4.1.0"): "8d214f5477365ffcbcbb7de058c86ed9a518efb7",
}


def collect(target, destination):
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--offline", "--format-version", "1",
        "--filter-platform", target,
    ], cwd=ROOT, text=True))
    manifest = []
    for package in sorted(metadata["packages"], key=lambda p: (p["name"], p["version"])):
        if package["name"] == "neordr":
            continue
        source = Path(package["manifest_path"]).parent
        output = destination / f"{package['name']}-{package['version']}"
        output.mkdir(parents=True)
        files = {path for path in source.rglob("*") if path.is_file() and
                 path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "UNLICENSE"))}
        if package.get("license_file"):
            files.add(source / package["license_file"])
        for path in sorted(files):
            relative = path.relative_to(source)
            (output / relative).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, output / relative)
        record = {key: package.get(key) for key in
                  ("name", "version", "license", "authors", "repository", "source")}
        record["license_files"] = [str(path.relative_to(source)) for path in sorted(files)]
        if not files:
            revision = OBJC2.get((package["name"], package["version"]))
            vcs = json.loads((source / ".cargo_vcs_info.json").read_text()) if revision else {}
            if not revision or vcs.get("git", {}).get("sha1") != revision:
                raise SystemExit(f"No license files packaged by {package['name']} {package['version']}; review upstream before release")
            # Upstream's workspace notice is omitted from these crate archives.
            # Retain that notice verbatim plus the standard MIT terms it links to.
            for filename in ("objc2-UPSTREAM.md", "MIT-template.txt"):
                shutil.copy2(ROOT / "licenses" / filename, output / filename)
                record["license_files"].append(filename)
            record["upstream_notice"] = f"https://github.com/madsmtm/objc2/blob/{revision}/LICENSE.md"
        manifest.append(record)
    (destination / "dependencies.json").write_text(json.dumps(manifest, indent=2) + "\n")


if __name__ == "__main__":
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    collect(args.target, args.output)
