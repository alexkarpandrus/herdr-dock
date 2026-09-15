#!/usr/bin/env python3

import sys
import tomllib
from pathlib import Path


def next_version(version: str, bump: str) -> str:
    try:
        major, minor, patch = map(int, version.split("."))
    except ValueError as error:
        raise ValueError(f"expected X.Y.Z version, got {version!r}") from error

    if bump == "major":
        return f"{major + 1}.0.0"
    if bump == "minor":
        return f"{major}.{minor + 1}.0"
    if bump == "patch":
        return f"{major}.{minor}.{patch + 1}"
    raise ValueError(f"unknown bump {bump!r}")


def replace_version(path: Path, old: str, new: str) -> None:
    text = path.read_text()
    needle = f'version = "{old}"'
    if text.count(needle) != 1:
        raise ValueError(f"expected one {needle!r} in {path}")
    path.write_text(text.replace(needle, f'version = "{new}"'))


def main() -> None:
    if sys.argv[1:] == ["--self-test"]:
        assert next_version("1.2.3", "major") == "2.0.0"
        assert next_version("1.2.3", "minor") == "1.3.0"
        assert next_version("1.2.3", "patch") == "1.2.4"
        return
    if len(sys.argv) != 2:
        raise SystemExit("usage: bump-version.py major|minor|patch")

    cargo_path = Path("Cargo.toml")
    plugin_path = Path("herdr-plugin.toml")
    cargo_version = tomllib.loads(cargo_path.read_text())["package"]["version"]
    plugin_version = tomllib.loads(plugin_path.read_text())["version"]
    if cargo_version != plugin_version:
        raise ValueError(
            f"Cargo.toml is {cargo_version}, herdr-plugin.toml is {plugin_version}"
        )

    version = next_version(cargo_version, sys.argv[1])
    replace_version(cargo_path, cargo_version, version)
    replace_version(plugin_path, plugin_version, version)
    print(version)


if __name__ == "__main__":
    main()
