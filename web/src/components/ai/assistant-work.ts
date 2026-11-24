// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ChatPart, ToolCall } from './chat-message-parts'
import { toolExecutionState } from './chat-message-parts'

/**
 * How an assistant answer reads: what the model says stays in the conversation
 * as it said it, and each run of tool calls between its words folds into one
 * line ("Ran 6 commands, read 2 files") that opens to every step. Cards that
 * need the user — a proposal to approve, a question, a secret to fill — never
 * fold away.
 */

/** One tool call of the work, in the order it happened. */
export interface WorkStep {
  tool: ToolCall
  index: number
}

export type AssistantSegment =
  | { kind: 'part'; part: ChatPart; index: number }
  | { kind: 'work'; key: string; steps: WorkStep[] }

/**
 * Splits an answer's parts into what is shown as it is and what folds into a
 * work group. Consecutive foldable tools become one group; text and cards are
 * never folded, so the model's words never move once they are on screen.
 * Blank text between two tools does not split their group.
 */
export function groupAssistantParts(
  parts: readonly ChatPart[],
  isFoldableTool: (tool: ToolCall) => boolean
): AssistantSegment[] {
  const segments: AssistantSegment[] = []
  let run: WorkStep[] = []

  const flush = () => {
    if (run.length > 0) {
      segments.push({ kind: 'work', key: `work-${run[0].index}`, steps: run })
    }
    run = []
  }

  parts.forEach((part, index) => {
    if (part.type === 'tool' && isFoldableTool(part.tool)) {
      run.push({ tool: part.tool, index })
    } else if (part.type === 'text' && !part.text.trim()) {
      return
    } else {
      flush()
      segments.push({ kind: 'part', part, index })
    }
  })
  flush()
  return segments
}

/**
 * The result an answer stands on, if it has one. An answer that only looked
 * things up ("list my projects") ends on its result, so the last result with a
 * view of its own stays on screen as that view. Once the model does anything
 * else (reads a playbook, runs a command, writes a file) the lookups were steps
 * toward that work, so there is none: they fold with the rest of it.
 */
export function standaloneResultId(
  parts: readonly ChatPart[],
  isLookup: (tool: ToolCall) => boolean,
  hasOwnView: (tool: ToolCall) => boolean
): string | null {
  const tools = parts.flatMap((part) =>
    part.type === 'tool' ? [part.tool] : []
  )
  if (!tools.every(isLookup)) return null
  const viewable = tools.filter(hasOwnView)
  return viewable.length > 0 ? viewable[viewable.length - 1].id : null
}

/**
 * The work group the turn is working on: the last segment, while the turn
 * runs. Once the model writes after it the group is done, and the activity
 * indicator follows the text instead.
 */
export function liveWorkSegmentKey(
  segments: readonly AssistantSegment[],
  streaming: boolean
): string | null {
  const last = segments[segments.length - 1]
  return streaming && last?.kind === 'work' ? last.key : null
}

type ToolKind =
  | 'command'
  | 'read'
  | 'edit'
  | 'search'
  | 'fetch'
  | 'process'
  | 'platform'
  | 'goal'
  | 'playbook'
  | 'sandbox'
  | 'other'

function baseToolName(name: string): string {
  const segments = name.split('__')
  return (segments[segments.length - 1] ?? name).toLowerCase()
}

function toolKind(tool: ToolCall): ToolKind {
  const name = baseToolName(tool.name)
  if (['sandbox_exec', 'bash', 'exec_command', 'shell'].includes(name))
    return 'command'
  if (['sandbox_read_file', 'read', 'view', 'notebookread'].includes(name))
    return 'read'
  if (
    [
      'sandbox_write_file',
      'write',
      'edit',
      'multiedit',
      'notebookedit',
      'apply_patch',
    ].includes(name)
  )
    return 'edit'
  if (['grep', 'glob', 'ls', 'websearch', 'web_search'].includes(name))
    return 'search'
  if (['webfetch', 'web_fetch', 'fetch'].includes(name)) return 'fetch'
  if (name.startsWith('temps_process_')) return 'process'
  if (name === 'temps') return 'platform'
  if (name === 'temps_goal') return 'goal'
  if (name === 'temps_playbook') return 'playbook'
  if (name === 'temps_create_sandbox') return 'sandbox'
  return 'other'
}

function parsedArguments(tool: ToolCall): Record<string, unknown> | null {
  try {
    const parsed: unknown = JSON.parse(tool.arguments)
    return parsed && typeof parsed === 'object'
      ? (parsed as Record<string, unknown>)
      : null
  } catch {
    // Still streaming, or not JSON: there is nothing to read from it yet.
    return null
  }
}

function argumentText(tool: ToolCall, keys: string[]): string | null {
  const args = parsedArguments(tool)
  if (!args) return null
  for (const key of keys) {
    const value = args[key]
    if (typeof value === 'string' && value.trim()) return value.trim()
  }
  return null
}

const PATH_KEYS = ['file_path', 'filePath', 'path', 'target_file']

const PHRASES: Record<ToolKind, [one: string, many: (n: number) => string]> = {
  command: ['ran a command', (n) => `ran ${n} commands`],
  read: ['read a file', (n) => `read ${n} files`],
  edit: ['edited a file', (n) => `edited ${n} files`],
  search: ['ran a search', (n) => `ran ${n} searches`],
  fetch: ['fetched a page', (n) => `fetched ${n} pages`],
  process: ['managed a process', (n) => `managed ${n} processes`],
  platform: ['checked Temps', (n) => `checked Temps ${n} times`],
  goal: ['set a goal', (n) => `updated goals ${n} times`],
  playbook: ['read a playbook', (n) => `read ${n} playbooks`],
  sandbox: ['started a sandbox', (n) => `started ${n} sandboxes`],
  other: ['used a tool', (n) => `used ${n} tools`],
}

const ORDER: ToolKind[] = [
  'command',
  'read',
  'edit',
  'search',
  'fetch',
  'process',
  'platform',
  'goal',
  'playbook',
  'sandbox',
  'other',
]

/**
 * What the tools did, as one line: "Ran 6 commands, read 2 files". Files are
 * counted once however often they were opened or edited.
 */
export function workSummary(tools: readonly ToolCall[]): string {
  const counts = new Map<ToolKind, number>()
  const files = new Map<ToolKind, Set<string>>()
  for (const tool of tools) {
    const kind = toolKind(tool)
    const path =
      kind === 'read' || kind === 'edit' ? argumentText(tool, PATH_KEYS) : null
    if (path) {
      const seen = files.get(kind) ?? new Set<string>()
      if (seen.has(path)) continue
      seen.add(path)
      files.set(kind, seen)
    }
    counts.set(kind, (counts.get(kind) ?? 0) + 1)
  }
  const phrases = ORDER.filter((kind) => counts.has(kind)).map((kind) => {
    const n = counts.get(kind) ?? 0
    const [one, many] = PHRASES[kind]
    return n === 1 ? one : many(n)
  })
  const text = phrases.join(', ')
  return text ? text[0].toUpperCase() + text.slice(1) : 'Worked'
}

/** How many of the tools failed. */
export function failedToolCount(tools: readonly ToolCall[]): number {
  return tools.filter((tool) => toolExecutionState(tool) === 'failed').length
}

function firstLine(text: string, max = 120): string {
  const line = text.split('\n')[0] ?? ''
  return line.length > max ? `${line.slice(0, max - 1)}…` : line
}

/** What a step is doing, as it is doing it: "Running npm test". */
export function toolActivity(tool: ToolCall): string | null {
  const kind = toolKind(tool)
  if (kind === 'command') {
    const command = argumentText(tool, ['command', 'cmd'])
    return command ? `Running ${firstLine(command)}` : 'Running a command'
  }
  if (kind === 'read') {
    const path = argumentText(tool, PATH_KEYS)
    return path ? `Reading ${path}` : 'Reading a file'
  }
  if (kind === 'edit') {
    const path = argumentText(tool, PATH_KEYS)
    return path ? `Editing ${path}` : 'Editing a file'
  }
  if (kind === 'search') return 'Searching'
  if (kind === 'fetch') return 'Fetching a page'
  if (kind === 'platform') {
    const { detail } = traceStep(tool)
    return detail ? `Running ${detail}` : 'Checking Temps'
  }
  if (kind === 'process') return 'Managing a process'
  if (kind === 'playbook') return 'Reading a playbook'
  if (kind === 'sandbox') return 'Starting a sandbox'
  return null
}

/** The tool still running in a group, if any: the step the header follows. */
export function runningTool(steps: readonly WorkStep[]): ToolCall | null {
  for (let at = steps.length - 1; at >= 0; at--) {
    const step = steps[at]
    if (toolExecutionState(step.tool) === 'running') {
      return step.tool
    }
  }
  return null
}

/**
 * The one line a working group shows while it is closed: the step running
 * now ("Running npm test"), or between steps the step it last took and how
 * far it has got ("Ran npm test · 3 steps so far"), so a pause while the model
 * thinks never hides what it was doing. Null before the first step, when
 * "Working" says it all.
 */
export function liveWorkStatus(steps: readonly WorkStep[]): string | null {
  const running = runningTool(steps)
  if (running)
    return toolActivity(running) ?? `Using ${humanName(running.name)}`
  const last = steps[steps.length - 1]
  if (!last) return null
  const count =
    steps.length === 1 ? '1 step so far' : `${steps.length} steps so far`
  return `${finishedStep(last.tool)} · ${count}`
}

const PAST_VERBS: Record<string, string> = {
  Run: 'Ran',
  Read: 'Read',
  Write: 'Wrote',
  Edit: 'Edited',
  Search: 'Searched',
  Fetch: 'Fetched',
  Temps: 'Ran',
  Playbook: 'Read playbook',
  'Create sandbox': 'Created sandbox',
}

/** A step that has finished, in the past tense: "Ran projects list". */
export function finishedStep(tool: ToolCall): string {
  const { verb, detail } = traceStep(tool)
  const past = PAST_VERBS[verb] ?? verb
  const text = detail
    ? `${past} ${detail}`
    : verb === 'Temps'
      ? 'Checked Temps'
      : past
  return toolExecutionState(tool) === 'failed' ? `Failed: ${text}` : text
}

export function stepTools(steps: readonly WorkStep[]): ToolCall[] {
  return steps.map((step) => step.tool)
}

/**
 * One row of the work trace: what kind of step it was, and what it touched.
 * `verb` reads as plain words ("Run", "Edit"); `detail` is the command, path
 * or name it acted on, never the raw arguments.
 */
export interface TraceStep {
  verb: string
  detail: string | null
  /** Lines written and replaced, for a step that changed a file. */
  added?: number
  removed?: number
}

function lineCount(text: unknown): number {
  if (typeof text !== 'string' || text.length === 0) return 0
  return text.replace(/\n$/, '').split('\n').length
}

function editCounts(
  args: Record<string, unknown>
): Pick<TraceStep, 'added' | 'removed'> {
  if (typeof args.content === 'string')
    return { added: lineCount(args.content) }
  const edits = Array.isArray(args.edits) ? args.edits : [args]
  let added = 0
  let removed = 0
  for (const edit of edits) {
    if (!edit || typeof edit !== 'object') continue
    const change = edit as Record<string, unknown>
    added += lineCount(change.new_string)
    removed += lineCount(change.old_string)
  }
  return added || removed ? { added, removed } : {}
}

function humanName(name: string): string {
  const words = name
    .replace(/^(temps|sandbox)_/, '')
    .replace(/[_-]+/g, ' ')
    .trim()
  return words ? words[0].toUpperCase() + words.slice(1) : name
}

const PROCESS_VERBS: Record<string, string> = {
  temps_process_start: 'Start',
  temps_process_start_extra: 'Start',
  temps_process_stop: 'Stop',
  temps_process_restart: 'Restart',
  temps_process_status: 'Status',
  temps_process_logs: 'Logs',
}

export function traceStep(tool: ToolCall): TraceStep {
  const kind = toolKind(tool)
  const name = baseToolName(tool.name)
  const args = parsedArguments(tool) ?? {}
  const text = (keys: string[]) => argumentText(tool, keys)
  switch (kind) {
    case 'command': {
      const command = text(['command', 'cmd'])
      return { verb: 'Run', detail: command ? firstLine(command) : null }
    }
    case 'read':
      return { verb: 'Read', detail: text(PATH_KEYS) }
    case 'edit':
      return {
        verb:
          name === 'write' || name === 'sandbox_write_file' ? 'Write' : 'Edit',
        detail: text(PATH_KEYS),
        ...editCounts(args),
      }
    case 'search':
      return {
        verb: 'Search',
        detail: text(['pattern', 'query', 'path']),
      }
    case 'fetch':
      return { verb: 'Fetch', detail: text(['url']) }
    case 'process':
      return {
        verb: `${PROCESS_VERBS[name] ?? 'Manage'} process`,
        detail: text(['name', 'process_id', 'program']),
      }
    case 'platform': {
      const command = text(['command'])
      if (command) return { verb: 'Temps', detail: firstLine(command) }
      const commands = Array.isArray(args.commands)
        ? args.commands.filter(
            (value): value is string =>
              typeof value === 'string' && Boolean(value.trim())
          )
        : []
      return {
        verb: 'Temps',
        detail: commands.length
          ? firstLine(commands.map((value) => value.trim()).join(' → '))
          : null,
      }
    }
    case 'goal':
      return {
        verb: args.action === 'retarget' ? 'Retarget goal' : 'Watch',
        detail: text(['title']),
      }
    case 'playbook':
      return { verb: 'Playbook', detail: text(['name']) }
    case 'sandbox':
      return { verb: 'Create sandbox', detail: text(['name', 'image']) }
    case 'other':
      return {
        verb: humanName(name),
        detail: text([
          'command',
          'query',
          'title',
          'name',
          'path',
          'description',
        ]),
      }
  }
}
