// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'

import type {
  ComposeHostStatusResponse,
  ComposeValidationReportResponse,
} from '@/api/client'

import {
  composeCommandLine,
  composeErrorClassHint,
  composeErrorClassLabel,
  composeHostWarning,
  composeValidationWarning,
} from './compose-check'

const supported: ComposeHostStatusResponse = {
  probed: true,
  available: true,
  supported: true,
  version: '2.40.0',
  minimum_version: '2.35.0',
  missing_flags: [],
  problem: null,
  checked_at: '2026-10-15T00:00:00Z',
}

describe('Compose host warning', () => {
  test('stays silent until the probe finishes and when the CLI is supported', () => {
    expect(composeHostWarning(undefined)).toBeNull()
    expect(composeHostWarning({ ...supported, probed: false })).toBeNull()
    expect(composeHostWarning(supported)).toBeNull()
  })

  test('uses the server explanation when there is one', () => {
    expect(
      composeHostWarning({
        ...supported,
        supported: false,
        version: '2.20.0',
        missing_flags: ['--no-env-resolution'],
        problem: 'Docker Compose 2.20.0 does not support --no-env-resolution',
      })
    ).toBe('Docker Compose 2.20.0 does not support --no-env-resolution')
  })

  test('names the minimum version for an old CLI', () => {
    const warning = composeHostWarning({
      ...supported,
      supported: false,
      version: '2.20.0',
    })
    expect(warning).toContain('2.20.0')
    expect(warning).toContain('v2.35.0')
  })

  test('reports a missing CLI', () => {
    expect(
      composeHostWarning({
        ...supported,
        available: false,
        supported: false,
        version: null,
      })
    ).toContain('not installed')
  })
})

describe('Compose validation warning', () => {
  const failed: ComposeValidationReportResponse = {
    project_id: 7,
    passed: false,
    error_class: 'interpolation',
    error: "required variable 'API_URL' is not set",
    outdated: false,
    checked_at: '2026-10-15T00:00:00Z',
  }

  test('is absent for a passing or missing report', () => {
    expect(composeValidationWarning(null)).toBeNull()
    expect(composeValidationWarning({ ...failed, passed: true })).toBeNull()
  })

  test('explains a failure with its class and remediation', () => {
    const warning = composeValidationWarning(failed)
    expect(warning?.title).toContain('variable interpolation')
    expect(warning?.detail).toContain('API_URL')
    expect(warning?.hint).toContain('${VAR:-value}')
  })

  test('marks a failure recorded by an older version', () => {
    expect(
      composeValidationWarning({ ...failed, outdated: true })?.title
    ).toContain('older Temps version')
  })
})

describe('Compose error classes', () => {
  test('the unsupported-version hint names the minimum version', () => {
    expect(composeErrorClassLabel('unsupported_compose_version')).toBe(
      'Docker Compose too old'
    )
    expect(composeErrorClassHint('unsupported_compose_version')).toContain(
      'v2.35.0'
    )
  })

  test('falls back when the class is unknown', () => {
    expect(composeErrorClassLabel(null)).toBe('Compose check failed')
    expect(composeErrorClassHint(undefined)).toBeNull()
  })
})

describe('Compose command line', () => {
  test('quotes only the arguments a shell would split', () => {
    expect(
      composeCommandLine([
        'docker',
        'compose',
        '-p',
        'temps-7-3',
        '-f',
        'my compose.yaml',
        'config',
        '--no-interpolate',
      ])
    ).toBe(
      "docker compose -p temps-7-3 -f 'my compose.yaml' config --no-interpolate"
    )
    expect(composeCommandLine(["it's"])).toBe(`'it'\\''s'`)
  })
})
