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

test('visitor enrichment by a deployment token is attributed to that token', () => {
  const markup = renderToStaticMarkup(
    <table>
      <tbody>
        <AuditLogItemRow
          id={3}
          audit_date={0}
          operation_type="VISITOR_ENRICHED"
          data={{
            actor_kind: 'deployment_token',
            deployment_token_id: 42,
            deployment_token_name: 'checkout-app',
            project_id: 5,
            visitor_row_id: 11,
            custom_data_keys: ['email', 'name', 'plan', 'company'],
            custom_data_key_count: 4,
          }}
        />
      </tbody>
    </table>
  )
  expect(markup).toContain('Deployment token · checkout-app')
  expect(markup).toContain('Deployment token #42')
  expect(markup).toContain('Enriched visitor 11 (email, name, plan, +1 more)')
  expect(markup).toContain('Analytics')
  expect(markup).not.toContain('>system<')
})

test('a malformed enrichment payload still renders the generic row', () => {
  const markup = renderToStaticMarkup(
    <table>
      <tbody>
        <AuditLogItemRow
          id={4}
          audit_date={0}
          operation_type="VISITOR_ENRICHED"
          data={{ actor_kind: 12, custom_data_keys: 'nope' }}
        />
      </tbody>
    </table>
  )
  expect(markup).toContain('Enriched a visitor')
  expect(markup).toContain('>system<')
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
