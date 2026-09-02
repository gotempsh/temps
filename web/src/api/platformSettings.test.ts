// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { buildPlatformSettingsUpdateBody } from './platformSettings'
import type { PlatformSettings } from './platformSettings'

describe('buildPlatformSettingsUpdateBody', () => {
  test('includes Docker registry configuration in the settings request', () => {
    const dockerRegistry = {
      enabled: true,
      registry_url: 'https://registry.example.test',
      username: 'registry-user',
      password: 'registry-token',
      tls_verify: true,
      ca_certificate: null,
    }

    const body = buildPlatformSettingsUpdateBody({
      docker_registry: dockerRegistry,
    } as PlatformSettings)

    expect(body.docker_registry).toEqual(dockerRegistry)
  })

  // The server replaces the whole AppSettings document and deserializes it with
  // `#[serde(default)]`, so a `cloud` block we never send reads as one reset to
  // its defaults. Omitting it used to wipe the operator's Cloud destination and
  // outbox ceiling (ADR-041) and both bulk-activation spend guards (ADR-042)
  // whenever any unrelated settings page was saved.
  test('round-trips the cloud block so an unrelated save cannot reset it', () => {
    const cloud = {
      backend_url: 'https://cloud.staging.example',
      telemetry_enabled: true,
      backups_enabled: false,
      notifications_enabled: false,
      telemetry_outbox_max_bytes: 1073741824,
      telemetry_bulk_anomaly_factor: 2,
      telemetry_bulk_rate_limit_spans_per_sec: 5000,
    }

    const body = buildPlatformSettingsUpdateBody({
      cloud,
      preview_domain: 'apps.example.test',
    } as PlatformSettings)

    expect(body.cloud).toEqual(cloud)
  })
})
