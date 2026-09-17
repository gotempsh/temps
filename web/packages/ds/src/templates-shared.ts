// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { type ReactNode } from 'react'
import { type State } from './status'

/* ────────────────────────────────────────────────────────────────────────
   The three console page templates. Every console screen is one of these
   (the fourth template, `Article`, is a page that is read: `article.tsx`). A screen
   that does not fit is a reason to extend a template, not to start from a
   blank div.

   Ledger    title · status line · filter (/) · actions · rows with j / k / ⏎ · footer
   Detail    title · status line · tabs with number keys · actions · body
   Settings  title · status line · sections · sticky save (⌘S) · danger zone
   ──────────────────────────────────────────────────────────────────────── */

// ── PageTitle ──────────────────────────────────────────────────────────

/**
 * Every screen starts with what it is: the title is the only 700-weight text
 * on a console screen, with the one or two facts that place it (environment,
 * current deploy, image, region) in mono beside it. Where it is belongs to
 * the shell header: inside a shell the title is also portalled into the
 * header breadcrumb as the current crumb (ancestors are the shell's), so a
 * detail page's trail ends in the resource's real name, never its id.
 * Outside a shell a `crumbs` prop renders the trail above the title. The
 * block carries its own top padding: it is the first thing under the header
 * and needs air, not a border, to separate from it.
 */
export type Crumb = { label: ReactNode; onClick?: () => void }

/**
 * Facts about one record, the grouped-list way: one row per fact, soft rule
 * between rows, the key on the left in muted 400 at a fixed 11rem, the value
 * on the right in ink (mono for identifiers, addresses, ids). Values wrap;
 * keys never do. `copy` puts a copy button after the value.
 */
export type KV = {
  k: string
  v: ReactNode
  mono?: boolean
  copy?: string
  state?: State
}

/**
 * What happened to one record, in order, as a vertical rail. Each event is
 * drawn by an icon that says what kind of event it was (queued, sent,
 * delivered, opened, bounced…), never by a coloured dot: a dot only says
 * "fine/not fine", the icon says what. The icon turns red for `state`
 * error and muted for idle/sampled; otherwise it is ink. The label is the
 * event word at 500, the note explains it at 400 muted, the time sits right
 * in mono. Callers own the icon vocabulary (see `MAIL_EVENT_ICONS` in the
 * email page) so the same event always gets the same icon across pages.
 */
export type TimelineItem = {
  t: string
  label: string
  icon?: ReactNode
  state?: State
  note?: ReactNode
}

// ── Pager ──────────────────────────────────────────────────────────────

/**
 * Pagination, one way everywhere. Server-side, page-numbered, matching the
 * API (default 20 per page, max 100). Lives in a list's footer, never as a
 * bar of numbered buttons:
 *   1–20 of 1,284 · ‹ prev · next › · 20 per page
 * The range is the fact, prev/next are the only moves (the filter and the
 * sort are for finding things, not paging to them), and the page size is a
 * plain select with the API's allowed values. `[` and `]` page from the
 * keyboard while a ledger has the focus. Filtering or sorting resets to
 * page 1, which the caller does in `onFilter` / `onSort`. Infinite scroll is
 * banned: an operator needs to say "page 3" to a colleague.
 */
export type Page = {
  page: number
  pageSize: number
  total: number
  onPage: (page: number) => void
  onPageSize?: (size: number) => void
  sizes?: readonly number[]
}

export const PAGE_SIZES = [20, 50, 100] as const

export type LedgerRow = {
  id: string
  state: State
  /**
   * What kind of record the row is (an app / worker / static project, a
   * database engine, a control plane / worker node, a span kind). Drawn in a
   * fixed 16px slot at the head of the first cell, and before the name on a
   * phone, in muted ink — never coloured, never the state. Required when the
   * list mixes kinds; omitted when the ledger's title already names the kind.
   */
  icon?: ReactNode
  /** Desktop cells, one per column. Use <Num>, <Status>, plain spans. */
  cells: ReactNode[]
  /** Phone rendering: name on the first line, the status note on the second. */
  mobile: ReactNode
  onOpen?: () => void
  /** Raw values per sortable column key. Numbers sort numerically, null sorts last. */
  sort?: Record<string, string | number | null | undefined>
}

/**
 * A column is a label, or a label with a sort key. Clicking a sortable header
 * cycles asc → desc → off (back to the ledger's default order, which is the
 * `hint`). Exactly one column sorts at a time; there is no multi-sort, the
 * filter box is for narrowing. Numeric columns right-align.
 */
export type LedgerColumn =
  string | { label: string; key?: string; numeric?: boolean }

export type LedgerSort = { key: string; dir: 'asc' | 'desc' } | null

/**
 * The aria props a `Field` computes for the control it labels. Take them with
 * the render-prop form and spread them onto the input:
 * `<Field label="branch" error={e}>{(c) => <Input {...c} />}</Field>`.
 */
export type FieldControl = {
  id: string
  'aria-describedby': string | undefined
  'aria-invalid': true | undefined
}
