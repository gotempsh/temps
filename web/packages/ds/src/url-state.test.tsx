// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { afterEach, expect, test } from 'bun:test'
import { MemoryRouter, useLocation, useNavigate } from 'react-router'
import { useUrlBoundText, useUrlSearchText, useUrlState } from './url-state'

import {
  act,
  cleanup,
  fireEvent,
  render,
  waitFor,
} from '@testing-library/react'
import { installBrowserWindow } from '../../../src/test/dom'

installBrowserWindow()
afterEach(cleanup)

function SearchBox() {
  const { get, patch } = useUrlState<'q' | 'page' | 'tab'>()
  const [text, setText] = useUrlBoundText(get('q') ?? '', (value) =>
    // The same two-step update the list pages used to make: search, then page.
    {
      patch({ q: value || undefined })
      patch({ page: undefined })
    }
  )
  const location = useLocation()
  return (
    <div>
      <input
        aria-label="search"
        value={text}
        onChange={(event) => setText(event.target.value)}
      />
      <output data-testid="search">{location.search}</output>
    </div>
  )
}

test('typing a multi-character term into a URL-bound search keeps the full value', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/list?tab=backups&page=3']}>
      <SearchBox />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  let typed = ''
  for (const char of 'qa-ui-qj14a') {
    typed += char
    fireEvent.change(input, { target: { value: typed } })
  }
  expect(input.value).toBe('qa-ui-qj14a')
  // The search and the page reset both landed, and untouched params survived.
  await waitFor(() => {
    const params = new URLSearchParams(
      view.getByTestId('search').textContent ?? ''
    )
    expect(params.get('q')).toBe('qa-ui-qj14a')
    expect(params.has('page')).toBe(false)
    expect(params.get('tab')).toBe('backups')
  })
  expect(input.value).toBe('qa-ui-qj14a')
  view.unmount()
})

test('patch applies every key of one call in a single update', async () => {
  function Patcher() {
    const { patch } = useUrlState<'q' | 'page'>()
    const location = useLocation()
    return (
      <div>
        <button onClick={() => patch({ q: 'abc', page: 2 })}>go</button>
        <output data-testid="search">{location.search}</output>
      </div>
    )
  }
  const view = render(
    <MemoryRouter initialEntries={['/list']}>
      <Patcher />
    </MemoryRouter>
  )
  await act(async () => {
    fireEvent.click(view.getByText('go'))
  })
  const params = new URLSearchParams(
    view.getByTestId('search').textContent ?? ''
  )
  expect(params.get('q')).toBe('abc')
  expect(params.get('page')).toBe('2')
  view.unmount()
})

test('an external URL change replaces the text in the box', async () => {
  function Linked() {
    const { get, patch } = useUrlState<'q'>()
    const [text] = useUrlBoundText(get('q') ?? '', (value) =>
      patch({ q: value })
    )
    return (
      <div>
        <input aria-label="search" value={text} readOnly />
        <button onClick={() => patch({ q: 'from-link' })}>apply</button>
      </div>
    )
  }
  const view = render(
    <MemoryRouter initialEntries={['/list']}>
      <Linked />
    </MemoryRouter>
  )
  await act(async () => {
    fireEvent.click(view.getByText('apply'))
  })
  expect((view.getByLabelText('search') as HTMLInputElement).value).toBe(
    'from-link'
  )
  view.unmount()
})

const settle = (ms: number) =>
  act(async () => {
    await new Promise((resolve) => setTimeout(resolve, ms))
  })

function Navigable({ settleMs }: { settleMs: number }) {
  const { get, patch } = useUrlState<'q' | 'page'>()
  const navigate = useNavigate()
  const [text, setText] = useUrlBoundText(
    get('q') ?? '',
    (value) => patch({ q: value || undefined }),
    { settleMs }
  )
  const location = useLocation()
  return (
    <div>
      <input
        aria-label="search"
        value={text}
        onChange={(event) => setText(event.target.value)}
      />
      <button onClick={() => navigate(-1)}>back</button>
      <button
        onClick={() => {
          setText('abc')
          patch({ q: undefined })
        }}
      >
        type-then-clear
      </button>
      <button onClick={() => patch({ page: 2 })}>page</button>
      <output data-testid="search">{location.search}</output>
    </div>
  )
}

test('Back that cancels in-flight typing makes the box show the URL again', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/list?q=a', '/list']} initialIndex={1}>
      <Navigable settleMs={30} />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  await act(async () => {
    fireEvent.change(input, { target: { value: 'a' } })
    fireEvent.change(input, { target: { value: 'ab' } })
    fireEvent.click(view.getByText('back'))
  })
  await settle(120)
  expect(view.getByTestId('search').textContent).toBe('?q=a')
  expect(input.value).toBe('a')
  view.unmount()
})

test('an external clear back to the original query makes the box show the URL', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/list']}>
      <Navigable settleMs={30} />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  await act(async () => {
    fireEvent.click(view.getByText('type-then-clear'))
  })
  await settle(120)
  expect(view.getByTestId('search').textContent).toBe('')
  expect(input.value).toBe('')
  view.unmount()
})

test('typing is not reverted while the URL is still catching up', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/list']}>
      <Navigable settleMs={500} />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  await act(async () => {
    fireEvent.change(input, { target: { value: 'slow' } })
  })
  await settle(60)
  expect(input.value).toBe('slow')
  expect(view.getByTestId('search').textContent).toBe('?q=slow')
  view.unmount()
})

test('a navigation that lands on the query already shown discards a cancelled patch', async () => {
  function Racer() {
    const { patch } = useUrlState<'q' | 'page'>()
    const navigate = useNavigate()
    const location = useLocation()
    return (
      <div>
        <button
          onClick={() => {
            patch({ q: 'abcd' })
            navigate('/list?q=abc', { replace: true })
          }}
        >
          race
        </button>
        <button onClick={() => patch({ page: 2 })}>page</button>
        <output data-testid="search">{location.search}</output>
      </div>
    )
  }
  const view = render(
    <MemoryRouter initialEntries={['/list?q=abc']}>
      <Racer />
    </MemoryRouter>
  )
  await act(async () => {
    fireEvent.click(view.getByText('race'))
  })
  expect(view.getByTestId('search').textContent).toBe('?q=abc')
  await act(async () => {
    fireEvent.click(view.getByText('page'))
  })
  const params = new URLSearchParams(
    view.getByTestId('search').textContent ?? ''
  )
  expect(params.get('q')).toBe('abc')
  expect(params.get('page')).toBe('2')
  view.unmount()
})

function SearchTextBox({ paramKey }: { paramKey?: string }) {
  const [text, setText, committed] = useUrlSearchText(paramKey, {
    resetKeys: ['page'],
  })
  const location = useLocation()
  return (
    <div>
      <input
        aria-label="search"
        value={text}
        onChange={(event) => setText(event.target.value)}
      />
      <output data-testid="search">{location.search}</output>
      <output data-testid="committed">{committed}</output>
    </div>
  )
}

const typeFast = (input: HTMLInputElement, value: string) => {
  let typed = ''
  for (const char of value) {
    typed += char
    fireEvent.change(input, { target: { value: typed } })
  }
}

test('useUrlSearchText keeps fast typing, writes the key and resets page', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/list?page=4&tab=a']}>
      <SearchTextBox />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  typeFast(input, 'qa-ui-qj14a')
  expect(input.value).toBe('qa-ui-qj14a')
  await waitFor(() => {
    const params = new URLSearchParams(
      view.getByTestId('search').textContent ?? ''
    )
    expect(params.get('q')).toBe('qa-ui-qj14a')
    expect(params.has('page')).toBe(false)
    expect(params.get('tab')).toBe('a')
  })
  expect(view.getByTestId('committed').textContent).toBe('qa-ui-qj14a')
  expect(input.value).toBe('qa-ui-qj14a')
  view.unmount()
})

test('useUrlSearchText restores the text from the URL on a fresh mount', () => {
  const view = render(
    <MemoryRouter initialEntries={['/list?find=pages']}>
      <SearchTextBox paramKey="find" />
    </MemoryRouter>
  )
  expect((view.getByLabelText('search') as HTMLInputElement).value).toBe(
    'pages'
  )
  view.unmount()
})

test('useUrlSearchText clearing the box removes the key from the URL', async () => {
  const view = render(
    <MemoryRouter initialEntries={['/list?q=abc&tab=a']}>
      <SearchTextBox />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  fireEvent.change(input, { target: { value: '' } })
  expect(input.value).toBe('')
  await waitFor(() => {
    const params = new URLSearchParams(
      view.getByTestId('search').textContent ?? ''
    )
    expect(params.has('q')).toBe(false)
    expect(params.get('tab')).toBe('a')
  })
  view.unmount()
})

test('useUrlSearchText with a shared urlState composes with a sibling patch in the same tick', async () => {
  function Shared() {
    const urlState = useUrlState<'q' | 'types'>()
    const [text, setText] = useUrlSearchText('q', { urlState })
    const location = useLocation()
    return (
      <div>
        <input
          aria-label="search"
          value={text}
          onChange={(event) => setText(event.target.value)}
        />
        <button onClick={() => urlState.patch({ types: 'npm' })}>npm</button>
        <output data-testid="search">{location.search}</output>
      </div>
    )
  }
  const view = render(
    <MemoryRouter initialEntries={['/scan']}>
      <Shared />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  // Type, then click a filter before the router has committed the search.
  await act(async () => {
    fireEvent.change(input, { target: { value: 'openssl' } })
    fireEvent.click(view.getByText('npm'))
  })
  await settle(1200)
  const params = new URLSearchParams(
    view.getByTestId('search').textContent ?? ''
  )
  expect(params.get('q')).toBe('openssl')
  expect(params.get('types')).toBe('npm')
  expect(input.value).toBe('openssl')
  view.unmount()
})

test('separate useUrlState instances compose a search edit and a sibling write in one tick', async () => {
  function Search() {
    const [text, setText] = useUrlSearchText('q')
    return (
      <input
        aria-label="search"
        value={text}
        onChange={(event) => setText(event.target.value)}
      />
    )
  }
  function Tabs() {
    const { patch } = useUrlState<'tab'>()
    const location = useLocation()
    return (
      <div>
        <button onClick={() => patch({ tab: 'history' })}>history</button>
        <output data-testid="search">{location.search}</output>
      </div>
    )
  }
  const view = render(
    <MemoryRouter initialEntries={['/runtime']}>
      <Search />
      <Tabs />
    </MemoryRouter>
  )
  const input = view.getByLabelText('search') as HTMLInputElement
  await act(async () => {
    fireEvent.change(input, { target: { value: 'oom' } })
    fireEvent.click(view.getByText('history'))
  })
  await settle(1200)
  const params = new URLSearchParams(
    view.getByTestId('search').textContent ?? ''
  )
  expect(params.get('q')).toBe('oom')
  expect(params.get('tab')).toBe('history')
  expect(input.value).toBe('oom')
  view.unmount()
})

test('independent routers do not share pending writes', async () => {
  function Writer({ name }: { name: string }) {
    const { patch } = useUrlState<string>()
    const location = useLocation()
    return (
      <div>
        <button onClick={() => patch({ [name]: name })}>{name}</button>
        <output data-testid={`${name}-search`}>{location.search}</output>
      </div>
    )
  }
  const view = render(
    <div>
      <MemoryRouter initialEntries={['/one']}>
        <Writer name="first" />
      </MemoryRouter>
      <MemoryRouter initialEntries={['/two']}>
        <Writer name="second" />
      </MemoryRouter>
    </div>
  )
  await act(async () => {
    fireEvent.click(view.getByText('first'))
    fireEvent.click(view.getByText('second'))
  })
  expect(view.getByTestId('first-search').textContent).toBe('?first=first')
  expect(view.getByTestId('second-search').textContent).toBe('?second=second')
  view.unmount()
})
