// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { SandboxBackendLabel } from './SandboxBackendLabel'

function render(backend: string) {
  return renderToStaticMarkup(<SandboxBackendLabel backend={backend} />)
}

describe('SandboxBackendLabel', () => {
  test('names microsandbox as a microVM, not a container', () => {
    const html = render('microsandbox')
    expect(html).toContain('>microsandbox<')
    expect(html).toContain('libkrun microVM (experimental)')
    expect(html).not.toContain('Namespaced container')
  })

  test('keeps the Docker and Firecracker labels', () => {
    expect(render('docker')).toContain('Namespaced container')
    expect(render('docker')).toContain('>Docker<')
    expect(render('firecracker')).toContain(
      'Hardware-virtualized microVM (KVM)'
    )
    expect(render('firecracker')).toContain('>Firecracker<')
  })

  test('shows an unknown backend verbatim', () => {
    const html = render('future-backend')
    expect(html).toContain('>future-backend<')
    expect(html).toContain('title="future-backend"')
  })
})
