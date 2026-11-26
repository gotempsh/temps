// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { PromptContext } from '@json-render/core'
import type { z } from 'zod'
import { VIEW_LIMITS as limits } from './limits'

interface ViewComponentEntry {
  props: z.ZodType
  slots: string[]
  description: string
}

/**
 * The fence language a view is written in. The same name json-render's own
 * instructions use, so a model that has met the library writes it unprompted.
 */
export const VIEW_FENCE = 'spec'

/**
 * What the assistant is told about views. Written here, beside the catalog it
 * lists, and sent by the server with every assistant turn, so it is short:
 * the format, the rules that keep a view truthful, and one line per component.
 *
 * It replaces json-render's own instructions, which describe state, actions
 * and forms the console does not offer and ask the model to fill a view with
 * realistic sample data. An assistant that reports on a real platform must
 * never do that.
 */
export function viewPromptTemplate(context: PromptContext): string {
  const components = (
    context.catalog as { components: Record<string, ViewComponentEntry> }
  ).components
  const list = Object.entries(components)
    .map(([name, entry]) => {
      const children = entry.slots.length > 0 ? ' [takes children]' : ''
      return `- ${name}: ${context.formatZodType(entry.props)}${children}. ${entry.description}`
    })
    .join('\n')

  return `## Views
When an answer is a list of records, a few figures, a measure over time, or the facts of one record, draw it instead of writing a Markdown table: say in one sentence what it shows, then add a \`\`\`${VIEW_FENCE} fenced block. The console draws the block with its own components. Reasons, explanations and next steps stay as prose, and prose does not repeat what the view shows.

Inside the fence write JSON Patch lines, one JSON object per line and nothing else:
\`\`\`${VIEW_FENCE}
{"op":"add","path":"/root","value":"main"}
{"op":"add","path":"/elements/main","value":{"type":"Stack","props":{},"children":["totals","services"]}}
{"op":"add","path":"/elements/totals","value":{"type":"Stats","props":{},"children":["running","failing"]}}
{"op":"add","path":"/elements/running","value":{"type":"Stat","props":{"label":"Running","value":"3","tone":"ok"},"children":[]}}
{"op":"add","path":"/elements/failing","value":{"type":"Stat","props":{"label":"Failing","value":"1","tone":"error"},"children":[]}}
{"op":"add","path":"/elements/services","value":{"type":"Table","props":{"columns":["Service","State","Last deployed"],"rows":[]},"children":[]}}
{"op":"add","path":"/elements/services/props/rows/-","value":[{"text":"web","href":"/projects/web"},{"text":"Running","tone":"ok"},"2 hours ago"]}
{"op":"add","path":"/elements/services/props/rows/-","value":["worker",{"text":"Failed","tone":"error"},"3 days ago"]}
\`\`\`

Rules:
- The first line sets \`/root\` to an element key. Each element is one line that adds \`/elements/<key>\` with \`type\`, \`props\` and \`children\` (element keys; \`[]\` when it has none). Every key named in \`children\` has a line of its own.
- A list in props (a Table's \`rows\`, a Trend's \`points\`, a Bars' \`items\`) starts as \`[]\`. Each entry is then a line of its own that adds to \`/elements/<key>/props/<list>/-\`. A Table row is one array with one cell per column, in column order.
- Pick the form by what the reader does with it: Stats for a few headline figures, a Trend for one measure over time, Bars to compare one amount across named things, a Table when each record has several facts, KeyValues for one record.
- Use only the components below, with exactly the props listed. No other fields: no state, actions, repeat or visibility.
- Show only values a tool returned in this conversation. Never invent, estimate, round up or pad a view; leave out what you did not read, and say in prose when a list is partial.
- \`tone\` marks a state: healthy, in progress, degraded, failing, stopped. Leave it out of a plain count or measurement.
- \`href\` is a console path beginning with \`/\`, built from the console links above with real ids or slugs. Anything else is shown as plain text.
- A Table holds at most ${limits.rows} rows and ${limits.columns} columns, and any one text ${limits.text} characters. When there is more, show the first and say in prose how many are left out.
- At most one view in an answer. None for a one-line answer, for a question, or when a tool refused.

Components:
${list}`
}
