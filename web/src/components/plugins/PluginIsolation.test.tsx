// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import type {
  PluginIsolationResponse,
  PluginSandboxResponse,
} from '../../api/client/types.gen'
import {
  isolationActionLabel,
  isolationSummary,
  oppositeIsolation,
} from '../../lib/plugin-isolation'
import {
  PluginIsolationChoice,
  PluginIsolationStatus,
  PluginSandboxNotice,
} from './PluginIsolation'

const plugin = (
  overrides: Partial<PluginIsolationResponse>
): PluginIsolationResponse => ({
  name: 'reports',
  sandboxed: false,
  changeable: true,
  ...overrides,
})

const sandbox = (
  overrides: Partial<PluginSandboxResponse>
): PluginSandboxResponse => ({
  configured: true,
  mode: 'container',
  plugins: [],
  ...overrides,
})

test('a plugin that runs as the Temps account is flagged, a contained one is not', () => {
  expect(isolationSummary('process')).toMatchObject({
    tone: 'warn',
    label: 'Not contained',
  })
  expect(isolationSummary('container')).toMatchObject({
    tone: 'ok',
    label: 'Contained',
  })
  expect(isolationSummary('separate_user').tone).toBe('ok')
  expect(isolationSummary(undefined).label).toBe('Not running')
})

test('the row status names the isolation and renders nothing for a stopped plugin', () => {
  const contained = renderToStaticMarkup(
    <PluginIsolationStatus
      isolation={plugin({ effective: 'container', sandboxed: true })}
    />
  )
  expect(contained).toContain('Contained')
  expect(contained).toContain('its own container')
  expect(
    renderToStaticMarkup(
      <PluginIsolationStatus isolation={plugin({ effective: 'process' })} />
    )
  ).toContain('Not contained')
  expect(
    renderToStaticMarkup(<PluginIsolationStatus isolation={plugin({})} />)
  ).toBe('')
  expect(
    renderToStaticMarkup(<PluginIsolationStatus isolation={undefined} />)
  ).toBe('')
})

test('the move offered is the opposite of what the plugin runs with, in at most three words', () => {
  expect(oppositeIsolation(plugin({ effective: 'process' }))).toBe('container')
  expect(oppositeIsolation(plugin({ effective: 'container' }))).toBe('process')
  expect(oppositeIsolation(undefined)).toBe('container')
  for (const target of ['container', 'process'] as const) {
    expect(isolationActionLabel(target).split(' ').length).toBeLessThanOrEqual(
      3
    )
  }
})

test('a server that cannot contain plugins says why instead of hiding the feature', () => {
  const html = renderToStaticMarkup(
    <PluginSandboxNotice
      unavailable={false}
      sandbox={sandbox({
        configured: false,
        mode: 'process',
        reason: 'plugin containers need a Linux server.',
      })}
    />
  )
  expect(html).toContain('Plugins are not contained on this server')
  expect(html).toContain('plugin containers need a Linux server.')
  expect(html).toContain('run it without a container')
})

test('an installed plugin that did not start is listed with its reason', () => {
  const html = renderToStaticMarkup(
    <PluginSandboxNotice
      unavailable={false}
      sandbox={sandbox({
        plugins: [
          plugin({ name: 'running', effective: 'container', sandboxed: true }),
          plugin({
            name: 'reports',
            requested: 'container',
            start_failure:
              "Plugin 'reports' asks for the control-plane database, which a plugin in a container is never given.",
          }),
        ],
      })}
    />
  )
  expect(html).toContain('1 installed plugin is not running')
  expect(html).toContain('reports:')
  expect(html).toContain('asks for the control-plane database')
  expect(html).not.toContain('running:')
})

test('nothing is shown when plugins are contained and all of them run', () => {
  expect(
    renderToStaticMarkup(
      <PluginSandboxNotice
        unavailable={false}
        sandbox={sandbox({
          plugins: [plugin({ effective: 'container', sandboxed: true })],
        })}
      />
    )
  ).toBe('')
  expect(
    renderToStaticMarkup(
      <PluginSandboxNotice unavailable={false} sandbox={undefined} />
    )
  ).toBe('')
})

test('a failed status read is reported, not rendered as "everything is fine"', () => {
  expect(
    renderToStaticMarkup(
      <PluginSandboxNotice unavailable sandbox={undefined} />
    )
  ).toContain('Plugin isolation is unknown')
})

test('the install choice always explains the opt-out and says when a container is impossible', () => {
  const available = renderToStaticMarkup(
    <PluginIsolationChoice
      id="choice"
      sandbox={sandbox({})}
      uncontained={false}
      onChange={() => {}}
    />
  )
  expect(available).toContain('Run without a container')
  expect(available).not.toContain('cannot run plugins in containers')

  const impossible = renderToStaticMarkup(
    <PluginIsolationChoice
      id="choice"
      sandbox={sandbox({
        configured: false,
        mode: 'process',
        reason: 'Temps itself runs inside a container.',
      })}
      uncontained={false}
      onChange={() => {}}
    />
  )
  expect(impossible).toContain('cannot run plugins in containers')
  expect(impossible).toContain('Temps itself runs inside a container.')
  expect(impossible).toContain('the installation is refused')
})
