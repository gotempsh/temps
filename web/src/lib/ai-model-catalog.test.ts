// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  agentVersionState,
  catalogFormToUpdate,
  catalogSettingsSchema,
  catalogSettingsToForm,
  catalogSourceLabel,
  formatPerMillion,
  formatTokens,
  isNewerVersion,
} from './ai-model-catalog'

const settings = {
  source_url: null,
  effective_source_url: 'https://catalog.example.com/model-catalog.json',
  auto_refresh: true,
  refresh_interval_hours: 24,
  discover_provider_models: true,
}

describe('catalogSourceLabel', () => {
  it('names the Temps catalog or the custom host', () => {
    expect(catalogSourceLabel(settings)).toBe('Temps catalog')
    expect(
      catalogSourceLabel({
        ...settings,
        source_url: 'https://mirror.example.com:8443/api.json?token=secret',
      })
    ).toBe('mirror.example.com:8443')
  })
})

describe('catalog settings form', () => {
  it('round-trips the settings and sends a blank URL as a reset', () => {
    const form = catalogSettingsToForm(settings)
    expect(form).toEqual({
      sourceUrl: '',
      refreshIntervalHours: '24',
      autoRefresh: true,
      discoverProviderModels: true,
    })
    expect(
      catalogFormToUpdate({ ...form, refreshIntervalHours: ' 12 ' })
    ).toEqual({
      source_url: '',
      refresh_interval_hours: 12,
      auto_refresh: true,
      discover_provider_models: true,
    })
  })

  it('accepts only https URLs and intervals from 1 to 168 hours', () => {
    const valid = catalogSettingsToForm(settings)
    expect(catalogSettingsSchema.safeParse(valid).success).toBe(true)
    for (const sourceUrl of [
      'http://mirror.example.com/a.json',
      'mirror.example.com',
    ]) {
      expect(
        catalogSettingsSchema.safeParse({ ...valid, sourceUrl }).success
      ).toBe(false)
    }
    for (const refreshIntervalHours of ['0', '169', '1.5', 'daily']) {
      expect(
        catalogSettingsSchema.safeParse({ ...valid, refreshIntervalHours })
          .success
      ).toBe(false)
    }
    expect(
      catalogSettingsSchema.safeParse({ ...valid, refreshIntervalHours: '168' })
        .success
    ).toBe(true)
  })
})

describe('formatting', () => {
  it('prints prices per 1M tokens and context sizes', () => {
    expect(formatPerMillion(null)).toBe('—')
    expect(formatPerMillion(0)).toBe('$0')
    expect(formatPerMillion(2.5)).toBe('$2.5')
    expect(formatPerMillion(15)).toBe('$15')
    expect(formatPerMillion(0.0125)).toBe('$0.0125')
    expect(formatTokens(200_000)).toBe('200K')
    expect(formatTokens(1_000_000)).toBe('1M')
    expect(formatTokens(1_048_576)).toBe('1M')
    expect(formatTokens(undefined)).toBe('—')
  })
})

describe('agentVersionState', () => {
  it('reports an update when the workspace is behind the latest release', () => {
    expect(
      agentVersionState(
        { latest_version: '2.1.330' },
        {
          installed_version: '2.1.263',
          latest_version: '2.1.330',
          update_available: true,
          updating: false,
        }
      )
    ).toEqual({ kind: 'behind', installed: '2.1.263', latest: '2.1.330' })
  })

  it('falls back to the registry when the workspace could not read the latest', () => {
    expect(
      agentVersionState(
        { latest_version: '1.19.6' },
        {
          installed_version: '1.18.23',
          latest_version: null,
          update_available: false,
          updating: false,
        }
      )
    ).toEqual({ kind: 'behind', installed: '1.18.23', latest: '1.19.6' })
  })

  it('is current, updating or unknown otherwise', () => {
    const workspace = {
      installed_version: '0.171.0',
      latest_version: '0.171.0',
      update_available: false,
      updating: false,
    }
    expect(agentVersionState({ latest_version: '0.171.0' }, workspace)).toEqual(
      {
        kind: 'current',
        installed: '0.171.0',
      }
    )
    expect(agentVersionState({}, { ...workspace, updating: true })).toEqual({
      kind: 'updating',
    })
    expect(
      agentVersionState({}, null, 'The workspace could not be inspected')
    ).toEqual({
      kind: 'unknown',
      reason: 'The workspace could not be inspected',
    })
  })

  it('compares release numbers, ignoring pre-release suffixes', () => {
    expect(isNewerVersion('2.1.330', '2.1.263')).toBe(true)
    expect(isNewerVersion('2.1.263', '2.1.263')).toBe(false)
    expect(isNewerVersion('2.1.263-beta.1', '2.1.263')).toBe(false)
    expect(isNewerVersion('unknown', '2.1.263')).toBe(false)
  })
})
