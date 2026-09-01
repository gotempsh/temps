# SPDX-FileCopyrightText: 2024-2026 Temps Contributors
# SPDX-License-Identifier: MIT OR Apache-2.0

"""Fluent manifest builder.

Mirrors: sdks/node/packages/plugin-sdk/src/manifest-builder.ts
"""

from __future__ import annotations

from temps_plugin.types import NavEntry, NavSection, PluginManifest


class ManifestBuilder:
    """Fluent builder for PluginManifest."""

    def __init__(self, name: str, version: str) -> None:
        self._manifest = PluginManifest(name=name, version=version)

    def display_name(self, name: str) -> ManifestBuilder:
        self._manifest.display_name = name
        return self

    def description(self, desc: str) -> ManifestBuilder:
        self._manifest.description = desc
        return self

    def requires_db(self, val: bool = True) -> ManifestBuilder:
        self._manifest.requires_db = val
        return self

    def health_path(self, path: str) -> ManifestBuilder:
        self._manifest.health_path = path
        return self

    def add_nav(
        self,
        label: str,
        icon: str,
        path: str,
        section: NavSection = NavSection.PLATFORM,
        order: int = 50,
    ) -> ManifestBuilder:
        self._manifest.nav.append(
            NavEntry(label=label, icon=icon, path=path, section=section, order=order)
        )
        return self

    def event(self, event_type: str) -> ManifestBuilder:
        self._manifest.events.append(event_type)
        return self

    def build(self) -> PluginManifest:
        return self._manifest


def create_manifest(name: str, version: str) -> ManifestBuilder:
    """Create a new ManifestBuilder."""
    return ManifestBuilder(name, version)
