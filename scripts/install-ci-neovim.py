#!/usr/bin/env python3
"""Install the reviewed official Neovim build into a disposable CI directory."""
import hashlib
import os
from pathlib import Path
import platform
import subprocess
import tempfile
import urllib.request

VERSION = "v0.12.5"
CHECKSUMS = {
    "nvim-linux-arm64": "1aa5ca085249580ae0f91eb14f27ec0919773ff2d99a163d03f3d6c21ac29725",
    "nvim-linux-x86_64": "bce0f56eda1f1b1db6eee8f4133d7a38813ea07933837dd1777411ca384c6875",
    "nvim-macos-arm64": "65fb000099e47ca1b762584c484cc833f40e30851a0ec450d4174e16317c1f9b",
    "nvim-macos-x86_64": "81f4518622cb059b450ee2e498c6a1082a222f6bd89589de5bbcf0c6a68aa3fd",
}


def main():
    system = {"Linux": "linux", "Darwin": "macos"}[platform.system()]
    arch = {"aarch64": "arm64", "arm64": "arm64", "x86_64": "x86_64"}[platform.machine()]
    name = f"nvim-{system}-{arch}"
    root = Path(tempfile.mkdtemp(prefix="neordr-nvim-", dir=os.environ.get("RUNNER_TEMP")))
    archive = root / f"{name}.tar.gz"
    url = f"https://github.com/neovim/neovim/releases/download/{VERSION}/{archive.name}"
    urllib.request.urlretrieve(url, archive)
    actual = hashlib.sha256(archive.read_bytes()).hexdigest()
    if actual != CHECKSUMS[name]:
        raise SystemExit(f"Neovim checksum mismatch: {actual}")
    subprocess.run(["tar", "-xzf", str(archive), "-C", str(root)], check=True)
    binary = root / name / "bin" / "nvim"
    subprocess.run([str(binary), "--version"], check=True)
    with open(os.environ["GITHUB_PATH"], "a") as paths:
        paths.write(f"{binary.parent}\n")


if __name__ == "__main__":
    main()
