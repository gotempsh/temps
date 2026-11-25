// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, it } from 'bun:test'
import {
  agentVersionState,
  catalogFormToUpdate,
  catalogSettingsSchema,
  catalogSettingsToForm,
  catalogSourceLabel,
  clampPage,
  formatPerMillion,
  formatTokens,
  isNewerVersion,
  latestReleaseVersion,
  newestVersion,
  releaseCheckOutcome,
  updateOutcome,
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
    expect(
      catalogSettingsSchema.safeParse({
        ...valid,
        sourceUrl: 'https://mirror.example.com/models@v1.json',
      }).success
    ).toBe(true)
    for (const sourceUrl of [
      'http://mirror.example.com/a.json',
      'mirror.example.com',
      'https://mirror.example.com/a.json?token=secret',
      'https://mirror.example.com/a.json#secret',
      'https://user:secret@mirror.example.com/a.json',
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

  it('shows a release the registry found even when the workspace cache is stale', () => {
    expect(
      agentVersionState(
        { latest_version: '2.1.2' },
        {
          installed_version: '2.1.1',
          latest_version: '2.1.1',
          update_available: false,
          updating: false,
        }
      )
    ).toEqual({ kind: 'behind', installed: '2.1.1', latest: '2.1.2' })
  })

  it('is unknown, not up to date, when no latest version could be looked up', () => {
    expect(
      agentVersionState(
        { latest_version: null },
        {
          installed_version: '1.2.3',
          latest_version: null,
          update_available: false,
          updating: false,
          detail: 'lookup failed',
        }
      )
    ).toEqual({ kind: 'unknown', installed: '1.2.3', reason: 'lookup failed' })
    expect(
      agentVersionState(
        { latest_version: null, error: 'The npm registry did not answer' },
        {
          installed_version: '1.2.3',
          latest_version: null,
          update_available: false,
          updating: false,
        }
      )
    ).toEqual({
      kind: 'unknown',
      installed: '1.2.3',
      reason: 'The npm registry did not answer',
    })
  })

  it('trusts whichever check ran last', () => {
    const workspace = {
      installed_version: '2.1.2',
      latest_version: '2.1.2',
      update_available: false,
      updating: false,
    }
    // The workspace was just updated after npm withdrew 2.1.3; the registry
    // answer is from before that.
    expect(
      agentVersionState(
        { latest_version: '2.1.3', checked_at: '2026-11-23T10:00:00Z' },
        { ...workspace, checked_at: '2026-11-23T12:00:00Z' }
      )
    ).toEqual({ kind: 'current', installed: '2.1.2' })
    // "Check for releases" ran after the workspace's cached check.
    expect(
      agentVersionState(
        { latest_version: '2.1.3', checked_at: '2026-11-23T12:00:00Z' },
        { ...workspace, checked_at: '2026-11-23T10:00:00Z' }
      )
    ).toEqual({ kind: 'behind', installed: '2.1.2', latest: '2.1.3' })
    expect(latestReleaseVersion({ latest_version: null }, workspace)).toBe(
      '2.1.2'
    )
  })

  it('picks the newer of two versions', () => {
    expect(newestVersion('2.1.1', '2.1.2')).toBe('2.1.2')
    expect(newestVersion('2.1.2', '2.1.1')).toBe('2.1.2')
    expect(newestVersion(null, '1.0.0')).toBe('1.0.0')
    expect(newestVersion('1.0.0', undefined)).toBe('1.0.0')
    expect(newestVersion(null, null)).toBeNull()
  })

  it('compares release numbers, ignoring pre-release suffixes', () => {
    expect(isNewerVersion('2.1.330', '2.1.263')).toBe(true)
    expect(isNewerVersion('2.1.263', '2.1.263')).toBe(false)
    expect(isNewerVersion('2.1.263-beta.1', '2.1.263')).toBe(false)
    expect(isNewerVersion('unknown', '2.1.263')).toBe(false)
  })
})

describe('agentVersionState after a withdrawn release', () => {
  it('ignores the workspace flag once a newer check says the release is gone', () => {
    // The workspace cached 1.1.0 at 10:00; npm withdrew it and an 11:00
    // registry check reports 1.0.0, which is what is installed.
    expect(
      agentVersionState(
        { latest_version: '1.0.0', checked_at: '2026-11-24T11:00:00Z' },
        {
          installed_version: '1.0.0',
          latest_version: '1.1.0',
          update_available: true,
          updating: false,
          checked_at: '2026-11-24T10:00:00Z',
        }
      )
    ).toEqual({ kind: 'current', installed: '1.0.0' })
  })

  it('never takes a failed registry lookup for a newer check', () => {
    // The registry last found 1.0.0; the workspace since found 1.1.0. A
    // lookup that times out at 11:00 keeps 1.0.0 with the failure's time:
    // that is no evidence 1.1.0 was withdrawn.
    const workspace = {
      installed_version: '1.0.0',
      latest_version: '1.1.0',
      update_available: true,
      updating: false,
      checked_at: '2026-11-24T10:00:00Z',
    }
    const failed = {
      latest_version: '1.0.0',
      checked_at: '2026-11-24T11:00:00Z',
      error: 'registry request timed out',
    }
    expect(agentVersionState(failed, workspace)).toEqual({
      kind: 'behind',
      installed: '1.0.0',
      latest: '1.1.0',
    })
    expect(latestReleaseVersion(failed, workspace)).toBe('1.1.0')
  })
})

describe('agentVersionState before the workspace was read', () => {
  it('is unchecked rather than unknown when nothing was read yet', () => {
    expect(
      agentVersionState(
        { latest_version: '1.1.0' },
        {
          installed_version: null,
          latest_version: null,
          update_available: false,
          updating: false,
          checked_at: null,
          detail: null,
        }
      )
    ).toEqual({ kind: 'unchecked' })
    // A failed read is not "unchecked": it says why.
    expect(
      agentVersionState(
        { latest_version: '1.1.0' },
        {
          installed_version: null,
          update_available: false,
          updating: false,
          checked_at: '2026-11-24T10:00:00Z',
          detail: 'the installed version could not be read from the workspace',
        }
      )
    ).toEqual({
      kind: 'unknown',
      reason: 'the installed version could not be read from the workspace',
    })
  })
})

describe('updateOutcome', () => {
  const release = { latest_version: '1.1.0', checked_at: null, error: null }

  it('is a success only when the workspace reports the new version', () => {
    expect(
      updateOutcome('Codex', release, {
        installed_version: '1.1.0',
        latest_version: '1.1.0',
        update_available: false,
        updating: false,
      })
    ).toEqual({ level: 'success', message: 'Codex updated to 1.1.0' })
    const stillBehind = updateOutcome('Codex', release, {
      installed_version: '1.0.0',
      latest_version: '1.1.0',
      update_available: true,
      updating: false,
    })
    expect(stillBehind.level).toBe('warning')
    expect(stillBehind.message).toContain('still reports 1.0.0')
    const unreadable = updateOutcome('Codex', release, {
      installed_version: null,
      latest_version: '1.1.0',
      update_available: false,
      updating: false,
      detail: 'the installed version could not be read from the workspace',
    })
    expect(unreadable.level).toBe('warning')
    expect(unreadable.message).toContain('could not be read')
    expect(unreadable.message).not.toContain('latest release')
  })

  it('checks against the registry release when the workspace lookup failed', () => {
    // A home install without npm: the updater exited zero, the workspace
    // could not look up the latest release, and the CLI is still old.
    const stale = updateOutcome('Codex', release, {
      installed_version: '1.0.0',
      latest_version: null,
      update_available: false,
      updating: false,
      detail: 'the latest version could not be looked up (exit 127)',
    })
    expect(stale).toEqual({
      level: 'warning',
      message:
        'Codex still reports 1.0.0 (latest 1.1.0); another copy may come first on PATH',
    })
    expect(
      updateOutcome('Codex', release, {
        installed_version: '1.1.0',
        latest_version: null,
        update_available: false,
        updating: false,
      })
    ).toEqual({ level: 'success', message: 'Codex updated to 1.1.0' })
  })

  it('names a pre-release instead of reporting the update as done', () => {
    const beta = updateOutcome('Codex', release, {
      installed_version: '1.1.0-beta.1',
      latest_version: '1.1.0',
      update_available: false,
      updating: false,
    })
    expect(beta).toEqual({
      level: 'warning',
      message:
        'Codex now reports 1.1.0-beta.1, a pre-release; the latest release 1.1.0 is not installed',
    })
    // The row still counts it as current, as the server does.
    expect(
      agentVersionState(release, {
        installed_version: '1.1.0-beta.1',
        latest_version: '1.1.0',
        update_available: false,
        updating: false,
        checked_at: '2026-11-25T09:00:00Z',
      }).kind
    ).toBe('current')
    // The exact release, with or without a leading v, is a success.
    expect(
      updateOutcome('Codex', release, {
        installed_version: 'v1.1.0',
        latest_version: '1.1.0',
        update_available: false,
        updating: false,
      }).level
    ).toBe('success')
  })

  it('never claims success when no latest release is known at all', () => {
    const unconfirmed = updateOutcome(
      'Codex',
      { latest_version: null, error: 'the registry did not answer' },
      {
        installed_version: '1.0.0',
        latest_version: null,
        update_available: false,
        updating: false,
      }
    )
    expect(unconfirmed.level).toBe('warning')
    expect(unconfirmed.message).toContain('could not be looked up to confirm')
  })
})

describe('releaseCheckOutcome', () => {
  it('reports lookups that failed instead of a plain success', () => {
    expect(releaseCheckOutcome([{ name: 'Codex' }])).toEqual({
      level: 'success',
      message: 'Checked for new releases',
    })
    expect(
      releaseCheckOutcome([
        { name: 'Claude Code', error: 'registry returned 503' },
        { name: 'Codex' },
      ])
    ).toEqual({
      level: 'warning',
      message: 'Releases could not be looked up for Claude Code',
    })
    expect(
      releaseCheckOutcome([
        { name: 'Claude Code', error: 'registry returned 503' },
        { name: 'Codex', error: 'registry returned 503' },
      ]).level
    ).toBe('error')
  })
})

describe('clampPage', () => {
  it('moves back to the last page that exists', () => {
    expect(clampPage(2, 10, 20)).toBe(1)
    expect(clampPage(3, 41, 20)).toBe(3)
    expect(clampPage(5, 41, 20)).toBe(3)
    expect(clampPage(2, 0, 20)).toBe(1)
    expect(clampPage(0, 30, 20)).toBe(1)
  })
})
