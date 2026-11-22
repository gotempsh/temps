// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { useEffect } from 'react'
import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { MemoryRouter, useLocation, useNavigate } from 'react-router'
import { installBrowserWindow } from '@/test/dom'
import { SandboxDetailTabs } from './SandboxDetailTabs'

installBrowserWindow()
afterEach(cleanup)

function Navigation() {
  const location = useLocation()
  const navigate = useNavigate()
  return (
    <>
      <output aria-label="Location">
        {location.search}
        {location.hash}
      </output>
      <button onClick={() => navigate(-1)}>Go back</button>
    </>
  )
}

function setup(
  entry = '/sandboxes/example',
  terminal = <input aria-label="Command draft" />
) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <Navigation />
      <SandboxDetailTabs
        panels={{
          terminal,
          files: <input aria-label="File draft" />,
          services: <p>Service grants</p>,
          network: <p>Outbound connections</p>,
          access: <input aria-label="Access draft" />,
          activity: <p>Lifecycle events</p>,
        }}
      />
    </MemoryRouter>
  )
}

test('tabs mount lazily, retain drafts and terminal lifetime, and support browser history', async () => {
  let mounts = 0
  let closes = 0
  function TerminalSession() {
    useEffect(() => {
      mounts++
      return () => {
        closes++
      }
    }, [])
    return <input aria-label="Command draft" />
  }
  const view = setup('/sandboxes/example?filter=kept', <TerminalSession />)
  expect(view.getAllByRole('tab')).toHaveLength(6)
  expect(view.queryByLabelText('File draft')).toBeNull()
  fireEvent.change(view.getByLabelText('Command draft'), {
    target: { value: 'ls -la' },
  })
  await act(async () =>
    fireEvent.mouseDown(view.getByRole('tab', { name: 'Files' }), {
      button: 0,
      ctrlKey: false,
    })
  )
  await waitFor(() =>
    expect(
      view.getByRole('tab', { name: 'Files' }).getAttribute('aria-selected')
    ).toBe('true')
  )
  expect(view.getAllByRole('tabpanel')).toHaveLength(1)
  expect(view.getByLabelText('Location').textContent).toBe(
    '?filter=kept&tab=files'
  )
  fireEvent.change(view.getByLabelText('File draft'), {
    target: { value: 'unsaved contents' },
  })
  await act(async () =>
    fireEvent.click(view.getByRole('button', { name: 'Go back' }))
  )
  expect(
    (view.getByRole('textbox', { name: 'Command draft' }) as HTMLInputElement)
      .value
  ).toBe('ls -la')
  expect(mounts).toBe(1)
  expect(closes).toBe(0)
  await act(async () =>
    fireEvent.mouseDown(view.getByRole('tab', { name: 'Files' }), {
      button: 0,
      ctrlKey: false,
    })
  )
  expect(
    (view.getByRole('textbox', { name: 'File draft' }) as HTMLInputElement)
      .value
  ).toBe('unsaved contents')
  view.unmount()
  expect(closes).toBe(1)
})

for (const [suffix, label] of [
  ['?tab=services', 'Services'],
  ['#sandbox-preview-access', 'Access'],
  ['?tab=unknown', 'Terminal'],
]) {
  test(`direct link ${suffix} selects ${label}`, () => {
    const view = setup(`/sandboxes/example${suffix}`)
    expect(
      view.getByRole('tab', { name: label }).getAttribute('aria-selected')
    ).toBe('true')
    expect(view.getAllByRole('tabpanel')).toHaveLength(1)
  })
}
