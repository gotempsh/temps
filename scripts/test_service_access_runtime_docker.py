#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Run worker and guest relay contracts in a disposable Linux Docker container.

This exercises Unix attachments and SDK enforcement, not the full Temps cluster.
Only this invocation's container is removed, including on failure.
"""
import argparse
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import uuid


def run(args, **kwargs):
    return subprocess.run(args, check=True, timeout=1200, **kwargs)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', default='rust:1-slim')
    args = parser.parse_args()
    if not shutil.which('docker'):
        print('SKIP: Docker is not installed')
        return
    try:
        subprocess.run(['docker', 'info'], check=True, timeout=15,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired):
        print('SKIP: Docker is unavailable')
        return
    # Never replace a pre-existing image tag. Pull only when it is absent.
    inspected = subprocess.run(['docker', 'image', 'inspect', args.image],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=15)
    if inspected.returncode:
        run(['docker', 'pull', args.image])
    root = Path(__file__).resolve().parents[1]
    name = 'temps-access-contract-' + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='temps-access-contract-') as scratch:
        workspace = Path(scratch)
        for crate in ('temps-service-access-runtime', 'temps-pty-agent'):
            relative = Path('crates') / crate
            shutil.copytree(root / relative, workspace / relative)
        shutil.copy2(root / 'Cargo.lock', workspace / 'Cargo.lock')
        shutil.copy2(root / 'clippy.toml', workspace / 'clippy.toml')
        shutil.copy2(root / 'rust-toolchain.toml', workspace / 'rust-toolchain.toml')
        source = (root / 'Cargo.toml').read_text()
        sections = re.findall(r'^\[([^\n]+)\]\n(.*?)(?=^\[|\Z)', source, re.M | re.S)
        manifest = '[workspace]\nresolver = "2"\nmembers = ["crates/temps-service-access-runtime", "crates/temps-pty-agent"]\n'
        for section, body in sections:
            if section in ('workspace.package', 'workspace.dependencies') or section.startswith('workspace.lints.'):
                manifest += f'\n[{section}]\n{body}'
        (workspace / 'Cargo.toml').write_text(manifest)
        try:
            run(['docker', 'create', '--name', name,
                 '--memory', '2g', '--cpus', '2', '--pids-limit', '256',
                 '--workdir', '/workspace', args.image,
                 'cargo', 'test', '--all-targets', '-p', 'temps-service-access-runtime',
                 '-p', 'temps-pty-agent', '-j', '2'])
            # Copy rather than bind-mount: also works with a remote Docker host.
            run(['docker', 'cp', str(workspace) + '/.', name + ':/workspace'])
            run(['docker', 'start', '--attach', name])
            result = subprocess.check_output(
                ['docker', 'inspect', '--format', '{{.State.ExitCode}}', name],
                text=True, timeout=15).strip()
            if result != '0':
                raise RuntimeError(f'Runtime contract container exited with {result}')
        finally:
            # This UUID belongs solely to this invocation; never use prune.
            subprocess.run(['docker', 'rm', '-f', name], timeout=30,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)



if __name__ == '__main__':
    main()
