# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Temps Plugin SDK for Python.

Build external plugins for the Temps platform that compile into
single self-contained binaries via PyInstaller.
"""

from temps_plugin.types import (
    PluginManifest,
    NavEntry,
    NavSection,
    PluginArgs,
    PluginEvent,
    AuthContext,
)
from temps_plugin.manifest_builder import ManifestBuilder, create_manifest
from temps_plugin.context import PluginContext
from temps_plugin.runtime import run_plugin
from temps_plugin.protocol import extract_auth_context

__all__ = [
    "PluginManifest",
    "NavEntry",
    "NavSection",
    "PluginArgs",
    "PluginEvent",
    "AuthContext",
    "ManifestBuilder",
    "create_manifest",
    "PluginContext",
    "run_plugin",
    "extract_auth_context",
]
