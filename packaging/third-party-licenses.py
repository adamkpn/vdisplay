#!/usr/bin/env python3
"""Writes THIRD-PARTY-LICENSES.txt: the license text of every crate compiled
into the vdisplay binary, plus the Rust standard library, as MIT and
Apache-2.0 require for redistributed binaries.

Build-only crates (proc macros and their dependencies) are left out: none of
their code ends up in the binary. For crates offered under "MIT OR ...",
the MIT text is used.

Run by `make notices`: python3 packaging/third-party-licenses.py <out.txt>
"""

import json
import os
import subprocess
import sys

TARGET = "aarch64-apple-darwin"  # the x86_64 slice has the same dependencies
HERE = os.path.dirname(os.path.abspath(__file__))
LICENSE_PREFIXES = ("LICENSE", "LICENCE", "COPYING", "COPYRIGHT")


def cargo(*args):
    return subprocess.run(
        ["cargo", *args], check=True, capture_output=True, text=True
    ).stdout


def runtime_crates():
    """(name, version) of the crates linked into the binary, root excluded."""
    tree = cargo(
        "tree", "--locked", "-e", "normal,no-proc-macro", "--target", TARGET,
        "--prefix", "none", "-f", "{p}",
    )
    crates = set()
    for line in tree.splitlines():
        parts = line.split()
        if len(parts) >= 2 and parts[1].startswith("v"):
            crates.add((parts[0], parts[1][1:]))
    return crates


def license_files(package):
    """The license files to include for a crate, MIT first when offered."""
    root = os.path.dirname(package["manifest_path"])
    names = sorted(
        f for f in os.listdir(root) if f.upper().startswith(LICENSE_PREFIXES)
    )
    notices = sorted(f for f in os.listdir(root) if f.upper().startswith("NOTICE"))
    mit = [f for f in names if "MIT" in f.upper()]
    if "MIT" in (package.get("license") or "") and mit:
        names = mit
    return [os.path.join(root, f) for f in names + notices]


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: third-party-licenses.py <out.txt>")
    meta = json.loads(cargo("metadata", "--format-version", "1", "--locked"))
    root_id = meta["resolve"]["root"]
    wanted = runtime_crates()
    packages = sorted(
        (p for p in meta["packages"]
         if (p["name"], p["version"]) in wanted and p["id"] != root_id),
        key=lambda p: p["name"],
    )
    missing = [p["name"] for p in packages if not license_files(p)]
    if missing:
        sys.exit(f"no license file found for: {', '.join(missing)}")

    out = [
        "vdisplay includes the following third-party software. Each is used",
        "under the license shown; where a choice is offered, under MIT.",
        "",
        "  Rust standard library (MIT OR Apache-2.0)",
    ]
    out += [f"  {p['name']} {p['version']} ({p['license']})" for p in packages]
    sections = [("Rust standard library", "MIT OR Apache-2.0",
                 "https://github.com/rust-lang/rust",
                 [os.path.join(HERE, "licenses", "rust-std-LICENSE-MIT.txt")])]
    sections += [(f"{p['name']} {p['version']}", p["license"],
                  p.get("repository") or "", license_files(p)) for p in packages]
    for title, spdx, repo, files in sections:
        out += ["", "=" * 76, f"{title} ({spdx})"]
        if repo:
            out.append(repo)
        for path in files:
            with open(path, encoding="utf-8", errors="replace") as f:
                out += ["", f.read().strip()]
    with open(sys.argv[1], "w", encoding="utf-8") as f:
        f.write("\n".join(out) + "\n")
    print(f"wrote {sys.argv[1]} ({len(packages)} crates + Rust standard library)")


if __name__ == "__main__":
    main()
