// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type { ComposeCheckReport } from '@/api/client'
import { ComposeCheckReportView } from './ComposeCheckReportView'

const failed: ComposeCheckReport = {
  passed: false,
  error_class: 'unsupported_compose_version',
  error:
    'Docker Compose on this host does not support the --no-env-resolution option',
  resolution_reasons: ['project override'],
  resolution_ran: true,
  command: {
    argv: [
      'docker',
      'compose',
      '-p',
      'temps-7-3',
      '-f',
      'compose.yaml',
      'config',
      '--no-env-resolution',
    ],
    exit_code: 16,
    stderr: 'unknown flag: --no-env-resolution',
  },
  unset_variables: ['API_URL'],
  referenced_variables: ['API_URL', 'APP_PORT'],
  notes: ['Linked service variables are not included in this check.'],
}

describe('ComposeCheckReportView', () => {
  test('a failure shows its class, remediation, command, exit code and stderr', () => {
    const html = renderToStaticMarkup(
      <ComposeCheckReportView report={failed} />
    )
    expect(html).toContain('Docker Compose too old')
    expect(html).toContain('v2.35.0')
    expect(html).toContain('Update Docker Compose')
    expect(html).toContain(
      'docker compose -p temps-7-3 -f compose.yaml config --no-env-resolution'
    )
    expect(html).toContain('exit code 16')
    expect(html).toContain('unknown flag: --no-env-resolution')
    expect(html).toContain('project override')
    expect(html).toContain('API_URL')
    expect(html).toContain('Linked service variables')
  })

  test('a pass says so and omits the remediation', () => {
    const html = renderToStaticMarkup(
      <ComposeCheckReportView
        report={{
          ...failed,
          passed: true,
          error_class: null,
          error: null,
          command: null,
          resolution_reasons: [],
          unset_variables: [],
          notes: [],
        }}
      />
    )
    expect(html).toContain('passes the deploy-time checks')
    expect(html).not.toContain('v2.35.0')
    expect(html).not.toContain('Update Docker Compose')
    expect(html).not.toContain('exit code')
  })

  test('a missing CLI offers the same installation guide', () => {
    const html = renderToStaticMarkup(
      <ComposeCheckReportView
        report={{ ...failed, error_class: 'compose_unavailable' }}
      />
    )
    expect(html).toContain('Update Docker Compose')
  })

  test('other Compose errors do not suggest upgrading the CLI', () => {
    const html = renderToStaticMarkup(
      <ComposeCheckReportView
        report={{ ...failed, error_class: 'interpolation' }}
      />
    )
    expect(html).not.toContain('Update Docker Compose')
  })
})
