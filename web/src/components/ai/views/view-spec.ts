// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { compileSpecStream, type Spec, type UIElement } from '@json-render/core'
import { viewCatalog, type ViewComponentName } from './catalog'
import { VIEW_LIMITS } from './limits'

/** A view that can be drawn, and how much of what was written it leaves out. */
export interface ParsedView {
  spec: Spec
  /**
   * How much of what was written is not in the view: lines that are not a
   * patch, unknown components, props that do not fit, children that are
   * missing, repeated or past the limit. While an answer streams these are
   * mostly lines yet to arrive.
   */
  dropped: number
}

/** An own property, so `constructor` and the like are never a component. */
function has(record: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(record, key)
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

/** A line that is one patch: an object with an operation and a path. */
function isPatchLine(line: string): boolean {
  try {
    const patch: unknown = JSON.parse(line)
    return (
      isRecord(patch) &&
      typeof patch.op === 'string' &&
      typeof patch.path === 'string'
    )
  } catch {
    return false
  }
}

/**
 * What the fenced block holds: patch lines, or one whole spec object. Lines
 * that are not a patch are skipped and counted: a table is written one row
 * per line, so a line the model got wrong costs that row, and the count is
 * how the view knows to say so.
 */
function rawSpec(
  source: string
): { spec: Record<string, unknown>; unreadable: number } | null {
  const trimmed = source.trim()
  if (!trimmed) return null
  if (trimmed.startsWith('{')) {
    try {
      const whole: unknown = JSON.parse(trimmed)
      if (isRecord(whole) && 'elements' in whole) {
        return { spec: whole, unreadable: 0 }
      }
    } catch {
      // Not one object: patch lines, the usual form.
    }
  }
  const lines = trimmed.split('\n').filter((line) => line.trim() !== '')
  try {
    return {
      spec: compileSpecStream(source),
      unreadable: lines.filter((line) => !isPatchLine(line)).length,
    }
  } catch {
    return null
  }
}

/** Components that draw from numbers: their `value` stays a number. */
const CHARTS: ReadonlySet<string> = new Set<ViewComponentName>([
  'Trend',
  'Bars',
])

/** A number written as text: digits, an optional sign and decimal part. */
const NUMBER = /^-?\d+(\.\d+)?$/

/**
 * Props as a model tends to write them, brought to what the catalog takes.
 * Nearly every value a view shows is text, so a number becomes its text, and
 * a text past the limit is cut with an ellipsis rather than costing the view
 * the element it belongs to. A chart is the exception: the `value` it draws
 * from stays a number, and one written in quotes is read as the number it is.
 * Nothing else is: "1,829" or "39%" is not a number to scale a bar by.
 */
function normalised(
  value: unknown,
  chart: boolean,
  key = '',
  depth = 0
): unknown {
  if (chart && key === 'value') {
    return typeof value === 'string' && NUMBER.test(value.trim())
      ? Number(value)
      : value
  }
  if (typeof value === 'number') return String(value)
  if (typeof value === 'string') {
    return value.length > VIEW_LIMITS.text
      ? `${value.slice(0, VIEW_LIMITS.text - 1)}…`
      : value
  }
  // Deeper than any component's props go: not a view's data.
  if (depth > 4) return undefined
  if (Array.isArray(value)) {
    return value.map((item) => normalised(item, chart, key, depth + 1))
  }
  if (isRecord(value)) {
    const record: Record<string, unknown> = {}
    for (const name of Object.keys(value)) {
      if (name === '__proto__') continue
      record[name] = normalised(value[name], chart, name, depth + 1)
    }
    return record
  }
  return value
}

/**
 * Keys that name an object's own machinery rather than a property: as an
 * element's key, `__proto__` would replace the prototype of the elements
 * object instead of adding an element. A view never needs them.
 */
const RESERVED_KEYS = new Set(['__proto__', 'constructor', 'prototype'])

/** One element, kept only if it is a catalog component with valid props. */
function element(value: unknown): UIElement | null {
  if (!isRecord(value) || typeof value.type !== 'string') return null
  if (!has(viewCatalog.data.components, value.type)) return null
  const definition =
    viewCatalog.data.components[value.type as ViewComponentName]
  const props = definition.props.safeParse(
    normalised(value.props ?? {}, CHARTS.has(value.type))
  )
  if (!props.success) return null
  const children =
    definition.slots.length > 0 && Array.isArray(value.children)
      ? value.children.filter((key): key is string => typeof key === 'string')
      : []
  // Only type, the props its schema names and children survive: a view has
  // no state, actions, visibility rules or repeats, whatever the block asked
  // for.
  return { type: value.type, props: props.data, children }
}

/**
 * The view a fenced block describes, or null when it describes none that can
 * be drawn. What a model wrote is untrusted and may be half written (it is
 * drawn while it streams), so the result is rebuilt rather than checked:
 * every element is a catalog component whose props passed its schema, its
 * children exist, nothing is reached twice (no cycles, no shared subtrees)
 * and the whole view stays within `VIEW_LIMITS`.
 */
export function parseViewSpec(source: string): ParsedView | null {
  const read = rawSpec(source)
  if (!read) return null
  const raw = read.spec
  if (typeof raw.root !== 'string' || !isRecord(raw.elements)) {
    return null
  }
  const written = raw.elements
  const elements: Record<string, UIElement> = {}
  // Every key claimed, kept apart from `elements` so that no key, however
  // it is spelled, can hide from it.
  const claimed = new Set<string>()
  let dropped = read.unreadable

  const keep = (key: string): boolean => {
    const parsed =
      !RESERVED_KEYS.has(key) &&
      !claimed.has(key) &&
      claimed.size < VIEW_LIMITS.elements &&
      has(written, key)
        ? element(written[key])
        : null
    if (!parsed) {
      dropped += 1
      return false
    }
    const wanted = parsed.children ?? []
    // Claimed before its children are visited, so a child naming an
    // ancestor finds the key taken and is dropped.
    claimed.add(key)
    const kept: UIElement = { ...parsed, children: [] }
    elements[key] = kept
    kept.children = wanted.filter(keep)
    return true
  }

  return keep(raw.root) ? { spec: { root: raw.root, elements }, dropped } : null
}
