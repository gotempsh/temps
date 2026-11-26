// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { defineCatalog, defineSchema } from '@json-render/core'
import { schema as reactSchema } from '@json-render/react/schema'
import { z } from 'zod'
import { VIEW_LIMITS } from './limits'
import { viewPromptTemplate } from './prompt'

/** The console's status vocabulary: the only colours a view may use. */
const tone = z.enum(['ok', 'running', 'warn', 'error', 'idle'])

const text = z.string().max(VIEW_LIMITS.text)
/** A console path; anything else is drawn as plain text. */
const href = z.string().max(VIEW_LIMITS.text).nullish()

/** The number a chart draws from: a plain, finite number. */
const amount = z.number().finite()

const cell = z.union([text, z.object({ text, href, tone: tone.nullish() })])

/**
 * Everything the assistant can draw in an answer. Read-only on purpose: a
 * view shows what a lookup found, with links into the console. Nothing here
 * changes anything, takes input or loads from outside the console, so what a
 * model writes can do no more than a paragraph could.
 *
 * The schema is json-render's React one with the console's own instructions
 * for the model in place of the library's, which ask for sample data.
 */
const viewSchema = defineSchema(() => reactSchema.definition, {
  promptTemplate: viewPromptTemplate,
})

export const viewCatalog = defineCatalog(viewSchema, {
  components: {
    Stack: {
      props: z.object({}),
      slots: ['default'],
      description:
        'A column of other components. Use it as the root when a view has more than one part.',
    },
    Stats: {
      props: z.object({}),
      slots: ['default'],
      description: 'A row of figures. Its children are Stat.',
    },
    Stat: {
      props: z.object({
        label: text,
        value: text,
        detail: text.nullish(),
        tone: tone.nullish(),
      }),
      slots: [],
      description:
        'One figure with its label, such as "Deployments today" and "14". `detail` is a few words under it; `tone` marks it good or bad.',
    },
    Status: {
      props: z.object({ tone, label: text }),
      slots: [],
      description:
        'A state badge. tone is ok, running, warn, error or idle; label is the state in a word or two.',
    },
    KeyValues: {
      props: z.object({
        items: z
          .array(z.object({ label: text, value: text, href }))
          .max(VIEW_LIMITS.items),
      }),
      slots: [],
      description:
        'The facts of one record as label and value rows. A value with `href` links to its console page.',
    },
    Table: {
      props: z.object({
        columns: z.array(text).min(1).max(VIEW_LIMITS.columns),
        rows: z
          .array(z.array(cell).max(VIEW_LIMITS.columns))
          .max(VIEW_LIMITS.rows),
      }),
      slots: [],
      description:
        'Records, one per row. A cell is a string, or {"text","href"} to link the record\'s console page, or {"text","tone"} for a state.',
    },
    Trend: {
      props: z.object({
        label: text,
        points: z
          .array(z.object({ at: text, value: amount }))
          .min(2)
          .max(VIEW_LIMITS.points),
        partial: z.boolean().nullish(),
      }),
      slots: [],
      description:
        'One measure over time as a line, such as visitors per day. `label` names the measure. Each point is a time (`at`, written as it should read, each one different) and its number. Set `partial` when the last point is a period still in progress.',
    },
    Bars: {
      props: z.object({
        items: z
          .array(
            z.object({
              label: text,
              value: amount.min(0),
              text: text.nullish(),
              href,
            })
          )
          .min(1)
          .max(VIEW_LIMITS.items),
      }),
      slots: [],
      description:
        'One amount compared across named things as ranked bars, such as views by channel. Largest first, unless the names have an order of their own. `value` is the number the bar is drawn from; `text` is how to print it when a plain number is not enough ("44.9%", "1.2 s").',
    },
    Callout: {
      props: z.object({
        tone: z.enum(['info', 'success', 'warning', 'error']),
        title: text,
        text,
      }),
      slots: [],
      description:
        'One notice the user should not miss, such as a failing backup.',
    },
    Link: {
      props: z.object({ label: text, href: z.string().max(VIEW_LIMITS.text) }),
      slots: [],
      description:
        'A button that opens a console page. label is a verb or short noun, three words at most.',
    },
  },
  actions: {},
})

export type ViewComponentName = keyof typeof viewCatalog.data.components
