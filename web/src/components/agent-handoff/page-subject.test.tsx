// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, describe, expect, test } from 'bun:test'
import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { Profiler, useState, type ReactNode } from 'react'
import {
  AiAssistantProvider,
  useAiAssistant,
} from '@/components/ai/AiAssistantContext'
import { installBrowserWindow } from '@/test/dom'
import type { HandoffSubject } from '@/lib/agent-handoff'
import {
  PageSubjectStoreContext,
  createPageSubjectStore,
  useDeclaredPageSubject,
  useHasPageSubject,
  usePageSubject,
  type PageSubjectStore,
} from './page-subject'

installBrowserWindow()
afterEach(cleanup)

const PROJECT = { id: 4, slug: 'storefront', name: 'storefront' }
const TRACE: HandoffSubject = {
  kind: 'trace',
  project: PROJECT,
  traceId: '4bf92f3577b34da6a3ce929d0e0e4736',
}

function logs(text?: string): HandoffSubject {
  return {
    kind: 'runtime_logs',
    mode: 'history',
    project: PROJECT,
    startTime: '2026-11-25T14:02:00.000Z',
    endTime: '2026-11-25T14:07:00.000Z',
    text,
  }
}

describe('createPageSubjectStore', () => {
  test('holds what the page declared and tells subscribers', () => {
    const store = createPageSubjectStore()
    let calls = 0
    const unsubscribe = store.subscribe(() => {
      calls += 1
    })

    expect(store.get()).toBeNull()
    store.set('page-a', TRACE)
    expect(store.get()).toEqual(TRACE)
    expect(calls).toBe(1)

    unsubscribe()
    store.clear('page-a')
    expect(store.get()).toBeNull()
    expect(calls).toBe(1)
  })

  test('a page clears only its own subject', () => {
    const store = createPageSubjectStore()
    store.set('page-a', TRACE)
    store.set('page-b', logs())

    // Page A unmounts after page B has already declared.
    store.clear('page-a')
    expect(store.get()).toEqual(logs())

    store.set('page-b', null)
    expect(store.get()).toBeNull()
  })
})

function harness(store: PageSubjectStore) {
  return function Harness({ children }: { children: ReactNode }) {
    return (
      <AiAssistantProvider>
        <PageSubjectStoreContext.Provider value={store}>
          {children}
        </PageSubjectStoreContext.Provider>
      </AiAssistantProvider>
    )
  }
}

function Page({
  subject,
  details,
}: {
  subject: HandoffSubject | null
  details?: string[]
}) {
  usePageSubject(subject, { assistantDetails: details })
  return null
}

describe('usePageSubject', () => {
  test('one declaration reaches the header store and the assistant alike', () => {
    const store = createPageSubjectStore()
    function Probe() {
      const { pageContext } = useAiAssistant()
      return (
        <>
          <output data-testid="assistant-label">{pageContext?.label}</output>
          <output data-testid="assistant-value">{pageContext?.value}</output>
        </>
      )
    }
    const Harness = harness(store)
    render(
      <Harness>
        <Page subject={TRACE} details={['Root span: GET /checkout.']} />
        <Probe />
      </Harness>
    )

    expect(store.get()).toEqual(TRACE)
    expect(screen.getByTestId('assistant-label').textContent).toBe('this trace')
    expect(
      screen.getByTestId('assistant-value').textContent?.split('\n')
    ).toEqual([
      'The user is viewing trace 4bf92f3577b34da6a3ce929d0e0e4736 in project "storefront" (ID 4) in the Temps console.',
      'Root span: GET /checkout.',
      'If they say "this trace"/"this span"/"these logs", they mean this one. You can pull details with the temps CLI: `traces get_trace --trace_id <id>` and `telemetry-logs query_logs --trace_id <id>`.',
    ])
  })

  test('declares nothing while the record is not loaded, and clears on unmount', () => {
    const store = createPageSubjectStore()
    const Harness = harness(store)
    const view = render(
      <Harness>
        <Page subject={null} />
      </Harness>
    )
    expect(store.get()).toBeNull()

    view.rerender(
      <Harness>
        <Page subject={TRACE} />
      </Harness>
    )
    expect(store.get()).toEqual(TRACE)

    view.unmount()
    expect(store.get()).toBeNull()
  })

  test('a subject rebuilt on every render is one declaration, not one per render', () => {
    const store = createPageSubjectStore()
    let notifications = 0
    store.subscribe(() => {
      notifications += 1
    })
    const Harness = harness(store)
    const view = render(
      <Harness>
        <Page subject={{ ...TRACE }} />
      </Harness>
    )
    const afterMount = notifications
    for (let i = 0; i < 5; i += 1) {
      view.rerender(
        <Harness>
          <Page subject={{ ...TRACE, project: { ...PROJECT } }} />
        </Harness>
      )
    }
    expect(notifications).toBe(afterMount)
  })

  test('the header does not re-render while a declared subject changes', () => {
    const store = createPageSubjectStore()
    const commits = { header: 0, panel: 0 }
    const count = (id: string) => {
      if (id === 'header') commits.header += 1
      else commits.panel += 1
    }

    function HeaderProbe() {
      useHasPageSubject()
      return null
    }
    function PanelProbe() {
      const subject = useDeclaredPageSubject()
      return (
        <output data-testid="panel-text">
          {subject?.kind === 'runtime_logs' && subject.mode === 'history'
            ? subject.text
            : ''}
        </output>
      )
    }
    function LogsPage() {
      const [text, setText] = useState('')
      usePageSubject(logs(text || undefined))
      return (
        <input
          aria-label="Search logs"
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
      )
    }
    const Harness = harness(store)
    render(
      <Harness>
        <Profiler id="header" onRender={count}>
          <HeaderProbe />
        </Profiler>
        <Profiler id="panel" onRender={count}>
          <PanelProbe />
        </Profiler>
        <LogsPage />
      </Harness>
    )
    const headerAfterMount = commits.header
    const panelAfterMount = commits.panel

    const search = screen.getByLabelText('Search logs')
    for (const text of ['p', 'pa', 'pay', 'paym', 'payment']) {
      fireEvent.change(search, { target: { value: text } })
    }

    // Five filter changes: the header heard none of them, the panel all five.
    expect(commits.header).toBe(headerAfterMount)
    expect(commits.panel).toBe(panelAfterMount + 5)
    expect(screen.getByTestId('panel-text').textContent).toBe('payment')
  })

  test('declares without an assistant provider around it', () => {
    // The viewers are rendered on their own too; a missing assistant must
    // not turn a declaration into a crash.
    const store = createPageSubjectStore()
    const view = render(
      <PageSubjectStoreContext.Provider value={store}>
        <Page subject={TRACE} details={['Root span: GET /checkout.']} />
      </PageSubjectStoreContext.Provider>
    )
    expect(store.get()).toEqual(TRACE)
    view.unmount()
    expect(store.get()).toBeNull()
  })

  test('moving to another page hands the declaration over without a gap owner mix-up', () => {
    const store = createPageSubjectStore()
    const Harness = harness(store)
    const view = render(
      <Harness>
        <Page key="trace" subject={TRACE} />
      </Harness>
    )
    view.rerender(
      <Harness>
        <Page key="logs" subject={logs('payment')} />
      </Harness>
    )
    expect(store.get()).toEqual(logs('payment'))
  })
})
