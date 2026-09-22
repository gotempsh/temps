#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Copy and SHA-256 verify durable local files before enabling stateless mode.

Stop the source control plane first. This script never deletes source files.
Use the same TEMPS_LOG_S3_* credentials as the destination installation.
Without --apply it only reports the file count and destination namespace.
Duplicate destination keys are rejected before any uploads; reconcile files
shared by cas/cache and static before retrying.
PostgreSQL must be backed up separately, with the original auth/encryption
secrets retained outside the backup. Git checkouts and caches are disposable.
"""

import argparse
import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys


def mappings(data_dir, log_dir):
    destinations = {}
    planned = []
    for folder, namespace in [(data_dir / "cas" / "blobs", "static-assets/blobs"),
                              (data_dir / "cas" / "cache", "static-assets/paths"),
                              (data_dir / "static", "static-assets/paths"),
                              (log_dir, "logs/build-logs")]:
        if folder.is_symlink():
            raise ValueError(f"Refusing symlink in source tree: {folder}")
        if not folder.exists():
            continue
        for source in sorted(folder.rglob("*")):
            if source.is_symlink():
                raise ValueError(f"Refusing symlink in source tree: {source}")
            if not source.is_file():
                continue
            relative = source.relative_to(folder).as_posix()
            key = f"{namespace}/{relative}"
            if key in destinations:
                raise ValueError(
                    f"Destination collision for {key}: {destinations[key]} and {source}; "
                    "resolve the source conflict before migration"
                )
            destinations[key] = source
            planned.append((source, key))
    # Validate the entire source tree before returning even the first upload.
    # Otherwise a late collision could leave a partially migrated destination.
    return planned


def digest(stream):
    result = hashlib.sha256()
    while chunk := stream.read(1024 * 1024):
        result.update(chunk)
    return result.digest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, required=True)
    parser.add_argument("--log-dir", type=Path, required=True,
                        help="Existing build/deploy LogService base directory")
    parser.add_argument("--instance-id", required=True)
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--source-stopped", action="store_true",
                        help="Assert source control plane is stopped and files are quiescent")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9._-]{1,64}", args.instance_id) or args.instance_id in (".", ".."):
        parser.error("invalid instance ID")
    if args.apply and not args.source_stopped:
        parser.error("--apply requires --source-stopped; stop the source before copying")
    for unsupported in ["ai-applications", "plugins", "plugin-data", "source-bundles", "static-bundles"]:
        folder = args.data_dir / unsupported
        if folder.exists() and any(folder.iterdir()):
            parser.error(f"{unsupported} contains local durable state unsupported by stateless v1; export or relocate it before migration")
    required = ["TEMPS_LOG_S3_BUCKET", "TEMPS_LOG_S3_ACCESS_KEY_ID", "TEMPS_LOG_S3_SECRET_ACCESS_KEY"]
    if any(not os.environ.get(name) for name in required):
        parser.error("set TEMPS_LOG_S3_BUCKET, TEMPS_LOG_S3_ACCESS_KEY_ID and TEMPS_LOG_S3_SECRET_ACCESS_KEY")
    env = os.environ.copy()
    env.update(AWS_ACCESS_KEY_ID=env["TEMPS_LOG_S3_ACCESS_KEY_ID"],
               AWS_SECRET_ACCESS_KEY=env["TEMPS_LOG_S3_SECRET_ACCESS_KEY"],
               AWS_DEFAULT_REGION=env.get("TEMPS_LOG_S3_REGION", "us-east-1"),
               AWS_PAGER="")
    command = ["aws"]
    if env.get("TEMPS_LOG_S3_ENDPOINT"):
        command.extend(["--endpoint-url", env["TEMPS_LOG_S3_ENDPOINT"]])
    prefix = f"s3://{env['TEMPS_LOG_S3_BUCKET']}/instances/{args.instance_id}/"
    count = 0
    for source, key in mappings(args.data_dir.resolve(), args.log_dir.resolve()):
        count += 1
        if not args.apply:
            continue
        before = source.stat()
        destination = prefix + key
        subprocess.run(command + ["s3", "cp", str(source), destination, "--only-show-errors"],
                       env=env, check=True, timeout=3600)
        with source.open("rb") as local:
            expected = digest(local)
        with subprocess.Popen(command + ["s3", "cp", destination, "-", "--only-show-errors"],
                              env=env, stdout=subprocess.PIPE) as remote:
            actual = digest(remote.stdout)
            if remote.wait(timeout=30) != 0:
                raise RuntimeError(f"Verification download failed for {key}")
        after = source.stat()
        if expected != actual or (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
            raise RuntimeError(f"Verification failed or source changed during copy: {key}")
    print(f"{'Copied and SHA-256 verified' if args.apply else 'Would copy'} {count} files to {prefix}")
    print("Source files retained. Keep the database backup and original secrets before switching configuration.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"Migration failed: {error}", file=sys.stderr)
        sys.exit(1)
