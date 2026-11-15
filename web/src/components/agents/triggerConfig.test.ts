// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { expect, test } from 'bun:test'
import { buildTriggerConfig, type EditableTriggers } from './triggerConfig'

const noEdits: EditableTriggers = {
  newIssue: false,
  regression: false,
  manual: false,
  cron: '',
}

test('saving the edit form keeps an existing webhook trigger', () => {
  // The form has no webhook control; omitting the key would make the
  // server delete the webhook and its token on an unrelated edit.
  const saved = buildTriggerConfig(
    { webhook: true, manual: true },
    { ...noEdits, manual: true }
  )
  expect(saved.webhook).toBe(true)
  expect(saved.manual).toBe(true)
})

test('triggers the form does not render are carried over unchanged', () => {
  const existing = {
    deploy: { production: true, preview: false },
    monitoring: { downtime: true },
    webhook: false,
  }
  expect(buildTriggerConfig(existing, noEdits)).toEqual(existing)
})

test('only the edited triggers the user enabled are written', () => {
  expect(buildTriggerConfig(null, noEdits)).toEqual({})
  expect(
    buildTriggerConfig(
      { error: { new_issue: true, regression: true }, manual: true },
      { newIssue: false, regression: true, manual: false, cron: '  0 * * * * ' }
    )
  ).toEqual({
    error: { new_issue: false, regression: true },
    schedule: { cron: '0 * * * *' },
  })
})
