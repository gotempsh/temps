// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { MemoryRouter } from 'react-router'

import type {
  IssueLinkView,
  OwnershipRuleView,
  SuspectCommitsView,
} from '@/api/client/types.gen'
import { IssueList, RepositoryMissing } from './GroupIssues'
import { rulesToForm } from './source-control'
import { SuspectCommitList } from './SuspectCommits'

const link = (overrides: Partial<IssueLinkView>): IssueLinkView => ({
  id: 5,
  group_id: 9,
  provider: 'github',
  repository: 'acme/shop',
  number: 12,
  url: 'https://git.example.test/acme/shop/issues/12',
  title: 'Checkout fails on empty carts',
  state: 'open',
  state_checked_at: '2026-11-20T10:00:00Z',
  created_by: 3,
  created_at: '2026-11-20T09:00:00Z',
  ...overrides,
})

describe('RepositoryMissing', () => {
  test('says what is missing and links to where it is fixed', () => {
    const html = renderToStaticMarkup(
      <MemoryRouter>
        <RepositoryMissing
          status={{
            configured: false,
            provider: null,
            repository: null,
            web_url: null,
            reason: 'Project 42 has no repository connected',
            setup_path: '/projects/shop/settings/git',
          }}
          example="Open issue files this error in the repository."
        />
      </MemoryRouter>
    )
    expect(html).toContain('Project 42 has no repository connected')
    expect(html).toContain('Open issue files this error in the repository.')
    expect(html).toContain('href="/projects/shop/settings/git"')
    expect(html).toContain('Connect a repository')
  })
})

describe('IssueList', () => {
  test('shows a repository issue with its state and a plain link by host', () => {
    const html = renderToStaticMarkup(
      <IssueList
        links={[
          link({}),
          link({
            id: 6,
            provider: 'link',
            repository: null,
            number: null,
            state: null,
            url: 'https://tracker.example.test/browse/SHOP-1',
            title: 'SHOP-1',
          }),
        ]}
        onUnlink={() => {}}
      />
    )
    expect(html).toContain('acme/shop#12')
    expect(html).toContain('Open')
    expect(html).toContain('tracker.example.test')
    expect(html).toContain('aria-label="Unlink SHOP-1"')
  })
})

describe('SuspectCommitList', () => {
  const result = (
    overrides: Partial<SuspectCommitsView>
  ): SuspectCommitsView => ({
    path: 'src/cart.ts',
    lineno: 41,
    function: 'total',
    at: '0123456789abcdef0123456789abcdef01234567',
    commits: [],
    reason: null,
    ...overrides,
  })

  test('names where the error fails and each commit', () => {
    const html = renderToStaticMarkup(
      <SuspectCommitList
        result={result({
          commits: [
            {
              sha: 'fedcba9876543210fedcba9876543210fedcba98',
              summary: 'Round cart totals',
              author_name: 'Ana',
              authored_at: null,
              url: 'https://git.example.test/acme/shop/commit/fedcba9',
            },
          ],
        })}
      />
    )
    expect(html).toContain('src/cart.ts:41')
    expect(html).toContain(' in total')
    expect(html).toContain('at 0123456')
    expect(html).toContain('Round cart totals')
    expect(html).toContain('fedcba9')
  })

  test('says why when no commit matches', () => {
    const html = renderToStaticMarkup(
      <SuspectCommitList
        result={result({
          path: null,
          lineno: null,
          function: null,
          at: null,
          reason: 'The latest event has no application frame in the repository',
        })}
      />
    )
    expect(html).toContain(
      'The latest event has no application frame in the repository'
    )
  })
})

describe('rulesToForm', () => {
  test('keeps the order and names an owner without a name by id', () => {
    const rule = (
      overrides: Partial<OwnershipRuleView>
    ): OwnershipRuleView => ({
      id: 1,
      position: 0,
      pattern: 'src/billing/',
      owner_user_id: 7,
      owner_name: 'Ana',
      created_at: '2026-11-20T09:00:00Z',
      updated_at: '2026-11-20T09:00:00Z',
      ...overrides,
    })
    expect(
      rulesToForm([
        rule({}),
        rule({
          id: 2,
          position: 1,
          pattern: '*.py',
          owner_user_id: 9,
          owner_name: null,
        }),
      ])
    ).toEqual({
      rules: [
        { pattern: 'src/billing/', owner_user_id: 7, owner_label: 'Ana' },
        { pattern: '*.py', owner_user_id: 9, owner_label: 'User #9' },
      ],
    })
  })
})
