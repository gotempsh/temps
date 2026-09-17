// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  BarChart3,
  Bot,
  Brain,
  Bug,
  Container,
  Database,
  FilePen,
  FileText,
  Gauge,
  GitBranch,
  Globe,
  HelpCircle,
  KeyRound,
  ListChecks,
  ListOrdered,
  Rocket,
  ScrollText,
  Search,
  Terminal,
  type LucideIcon,
} from 'lucide-react'
import { type State } from './status'

/* ────────────────────────────────────────────────────────────────────────
   Agent primitives: what an AI renders, and what proves it.

   The rule these enforce (brand-guidelines §0, "AI-native, under policy"):
   an agent is an operator, so its work is a ledger of typed tool calls with
   state words, every block it generates carries the call that produced it,
   and every write is proposed, never performed. Nothing here can draw a
   chat bubble, an avatar, or a block without a source.

   See `design-system/docs/generative-ui.md`. The reference surface is
   `/agent`.
   ──────────────────────────────────────────────────────────────────────── */

// ── Vocabulary ─────────────────────────────────────────────────────────

/** The six states of a tool call, plus the two approval outcomes. */
export type ToolState =
  | 'input-streaming'
  | 'input-available'
  | 'output-available'
  | 'output-error'
  | 'approval-requested'
  | 'approval-responded'
  | 'output-denied'

/** State glyph and state word for each. The word is never invented at a call site. */
export const TOOL_STATE: Record<ToolState, { state: State; word: string }> = {
  'input-streaming': { state: 'idle', word: 'preparing' },
  'input-available': { state: 'warn', word: 'running' },
  'output-available': { state: 'ok', word: 'done' },
  'output-error': { state: 'error', word: 'failed' },
  'approval-requested': { state: 'warn', word: 'needs approval' },
  'approval-responded': { state: 'ok', word: 'approved' },
  'output-denied': { state: 'idle', word: 'denied' },
}

/** What the call IS. One concept, one icon — the table in `docs/icons.md`. */
export type ToolKind =
  | 'command'
  | 'edit'
  | 'read'
  | 'search'
  | 'fetch'
  | 'git'
  | 'subagent'
  | 'reasoning'
  | 'plan'
  | 'tasks'
  | 'question'
  | 'query'
  | 'metrics'
  | 'errors'
  | 'logs'
  | 'deploy'
  | 'database'
  | 'config'
  | 'service'

export const KIND_ICON: Record<ToolKind, LucideIcon> = {
  command: Terminal,
  edit: FilePen,
  read: FileText,
  search: Search,
  fetch: Globe,
  git: GitBranch,
  subagent: Bot,
  reasoning: Brain,
  plan: ListOrdered,
  tasks: ListChecks,
  question: HelpCircle,
  query: BarChart3,
  metrics: Gauge,
  errors: Bug,
  logs: ScrollText,
  deploy: Rocket,
  database: Database,
  config: KeyRound,
  service: Container,
}

/**
 * Tool name → kind. The console's assistant calls two virtual tools, `temps`
 * (read) and `temps_write` (propose), each dispatching to one allowlisted API
 * operation — so the name a row shows is the operation, and that is what this
 * maps. Coding-agent tool names are here too. Unknown names read as a command,
 * which is the honest default.
 */
export const TOOL_KIND: Record<string, ToolKind> = {
  // coding agent
  run_command: 'command',
  bash: 'command',
  edit_file: 'edit',
  write_file: 'edit',
  apply_patch: 'edit',
  read_file: 'read',
  grep: 'search',
  glob: 'search',
  fetch: 'fetch',
  web_search: 'fetch',
  git: 'git',
  // console assistant · reads
  query_traces: 'query',
  get_trace: 'query',
  get_visitor_stats: 'query',
  get_events_timeline: 'query',
  query_metrics: 'metrics',
  get_metrics_over_time: 'metrics',
  get_container_metrics: 'metrics',
  list_error_groups: 'errors',
  get_error_group: 'errors',
  get_error_time_series: 'errors',
  query_logs: 'logs',
  get_container_logs: 'logs',
  get_deployment_job_logs: 'logs',
  get_deployment: 'deploy',
  get_project_deployments: 'deploy',
  get_last_deployment: 'deploy',
  get_environment_variables: 'config',
  get_resolved_environment_variables: 'config',
  list_containers: 'service',
  get_container_info: 'service',
  read_entity_rows: 'database',
  list_entities: 'database',
  // console assistant · writes (proposed, never performed)
  rollback_to_deployment: 'deploy',
  promote_deployment: 'deploy',
  trigger_project_pipeline: 'deploy',
  restart_container: 'service',
  stop_container: 'service',
  start_container: 'service',
  create_environment_variable: 'config',
  update_environment_variable: 'config',
  delete_environment_variable: 'config',
}

/** The kind of a call, from its name and its first argument. */
export function toolKind(name: string, arg?: string): ToolKind {
  if ((name === 'run_command' || name === 'bash') && arg?.startsWith('git '))
    return 'git'
  return TOOL_KIND[name] ?? 'command'
}

export function toolIcon(kind: ToolKind): LucideIcon {
  return KIND_ICON[kind]
}

// ── ToolRow ────────────────────────────────────────────────────────────

export type ToolApproval = {
  /** What it does, to whom, and how to undo it. Ends in "cannot be undone" when it cannot. */
  reason: string
  /** Irreversible loss only. Red left rule, red confirm, no "always". */
  destructive?: boolean
  onRespond: (r: 'once' | 'session' | 'deny') => void
}

// ── Streaming ──────────────────────────────────────────────────────────

/** Which block the skeleton stands in for. It must match what lands. */
export type StreamKind =
  'text' | 'chart' | 'ledger' | 'detail' | 'keyvalue' | 'tool'
