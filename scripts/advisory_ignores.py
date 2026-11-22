# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Check both ignore lists against the unfiltered cargo-audit report."""

import json
import os
import subprocess
import sys
import tomllib


def problems(workflow, deny, report):
    matched = {item["advisory"]["id"] for item in report["vulnerabilities"]["list"]}
    for warnings in report["warnings"].values():
        matched.update(item["advisory"]["id"] for item in warnings if item.get("advisory"))

    for advisory in sorted(workflow ^ deny):
        location = "AUDIT_IGNORE" if advisory in workflow else "deny.toml"
        yield f"{advisory} appears only in {location}. Synchronize both ignore lists."
    for advisory in sorted(workflow - matched):
        yield f"{advisory} matches no locked package. Remove both ignores and their notes."


def main():
    audit = subprocess.run(
        ["cargo", "audit", "--file", "Cargo.lock", "--json", "--no-fetch"],
        stdout=subprocess.PIPE,
    )
    # cargo-audit exits 1 for vulnerabilities, which this report must include.
    if audit.returncode not in (0, 1) or not audit.stdout.strip():
        sys.exit(f"Unfiltered cargo audit failed (exit {audit.returncode}).")
    report = json.loads(audit.stdout)
    with open("deny.toml", "rb") as config:
        deny = {entry["id"] for entry in tomllib.load(config)["advisories"]["ignore"]}
    workflow = set(filter(None, os.environ["AUDIT_IGNORE"].split(",")))
    errors = list(problems(workflow, deny, report))
    if errors:
        sys.exit("\n".join(errors))


if __name__ == "__main__":
    main()
