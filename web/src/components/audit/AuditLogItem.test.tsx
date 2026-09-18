// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { AuditLogItemRow } from './AuditLogItem'

test('autonomous plugin activity is attributed to its plugin, not system', () => {
  const markup = renderToStaticMarkup(
    <table>
      <tbody>
        <AuditLogItemRow
          id={1}
          audit_date={0}
          operation_type="EXTERNAL_PLUGIN_HOST_OPERATION_DENIED"
          data={{
            actor: { id: 'actor-id', kind: 'plugin', name: 'example-plugin' },
            operation: 'generate_ai',
            outcome: 'denied',
          }}
        />
      </tbody>
    </table>
  )
  expect(markup).toContain('example-plugin')
  expect(markup).toContain('Plugin actor actor-id')
  expect(markup).toContain('Denied plugin generate ai')
  expect(markup).not.toContain('>system<')
})

test.each([
  ['SUCCEEDED', 'Completed'],
  ['FAILED', 'Failed'],
])('plugin completion outcome %s is visible', (outcome, label) => {
  const markup = renderToStaticMarkup(
    <table>
      <tbody>
        <AuditLogItemRow
          id={2}
          audit_date={0}
          operation_type={`EXTERNAL_PLUGIN_HOST_OPERATION_${outcome}`}
          data={{
            actor: { id: 'actor-id', kind: 'plugin', name: 'example-plugin' },
            operation: 'GenerateAi',
          }}
        />
      </tbody>
    </table>
  )
  expect(markup).toContain(`${label} plugin generate ai`)
})
