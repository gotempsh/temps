// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { AiHarnessState, AiHarnessStatusResponse } from '@/api/client'
import type { Choice } from './ChoiceList'

/**
 * Agents that can load the Temps skill, by the id their logo is under.
 * Temps signs in to no harness of its own (ADR-116): the agent you already
 * use drives Temps through the skill and an API key its login creates.
 */
export const CONNECTABLE_AGENTS: Array<{ id: string; name: string }> = [
  { id: 'claude_cli', name: 'Claude Code' },
  { id: 'codex_cli', name: 'Codex' },
  { id: 'cursor', name: 'Cursor' },
  { id: 'github_copilot', name: 'GitHub Copilot' },
  { id: 'opencode', name: 'OpenCode' },
  { id: 'windsurf', name: 'Windsurf' },
]

/**
 * The line under the commands while your agent has not logged in. With no
 * key yet that is expected: the login creates it, so the step waits rather
 * than asking for a key.
 */
export function agentWaitingLine(
  state: AiHarnessState | undefined,
  reason: string | null | undefined
): string {
  if (state === 'waiting' && reason) return reason
  return 'Waiting for your agent to log in…'
}

export type AgentPath = 'coding_agent' | 'claude_app' | 'other_app'

/**
 * How the agent reaches this console. A coding agent runs a shell, so the
 * Temps CLI logs it in. The Claude app's shell runs in a VM in the cloud
 * that cannot reach a console on localhost or a tailnet, so it gets an
 * extension that Claude runs on this computer instead.
 */
export const AGENT_PATHS: Choice<AgentPath>[] = [
  {
    id: 'coding_agent',
    label: 'Coding agent',
    detail: 'Claude Code, Codex, Copilot and more. Paste one prompt.',
  },
  {
    id: 'claude_app',
    label: 'Claude app',
    detail: 'Install the Temps extension. Works on localhost too.',
  },
  {
    id: 'other_app',
    label: 'Another app',
    detail: 'Cursor, VS Code, Windsurf or Zed.',
  },
]

/** What the step says about the connection, for the path the user picked. */
export type AgentConnection =
  | { kind: 'connected' }
  | { kind: 'waiting'; line: string; canContinue: boolean }

/**
 * The status the server reports names the agent credential used most
 * recently. On an MCP app path (the Claude app, another app), another agent
 * being connected (an earlier `temps login`) is not proof the app works: the
 * step keeps waiting for an MCP key to be used, but lets the user continue.
 */
export function agentConnection(
  path: AgentPath,
  data:
    | Pick<AiHarnessStatusResponse, 'status' | 'reason' | 'connected_via'>
    | undefined
): AgentConnection {
  if (path === 'coding_agent') {
    return data?.status === 'connected'
      ? { kind: 'connected' }
      : {
          kind: 'waiting',
          line: agentWaitingLine(data?.status, data?.reason),
          canContinue: false,
        }
  }
  if (
    data?.connected_via?.kind === 'mcp_client' &&
    data.status === 'connected'
  ) {
    return { kind: 'connected' }
  }
  const app = path === 'claude_app' ? 'the Claude app' : 'your app'
  if (data?.status === 'connected') {
    return {
      kind: 'waiting',
      line: `Waiting for ${app} to call Temps. Another agent is already connected, so you can continue without it.`,
      canContinue: true,
    }
  }
  return {
    kind: 'waiting',
    line: `Waiting for ${app} to call Temps…`,
    canContinue: false,
  }
}

/** Whether the MCP server is on, as far as this user can tell. */
export type McpServerState = 'loading' | 'on' | 'off' | 'unknown'

/**
 * Why the last step of an MCP app setup isn't available yet, or null when it
 * is. The app needs the MCP server on and a key: set up without them, it
 * fails on its first call. A user who can't read the settings ('unknown') is
 * told an admin turns the server on, and only the key gates them. `next`
 * names the last step ("download the extension").
 */
export function mcpSetupBlocker(
  mcp: McpServerState,
  keyCreated: boolean,
  next: string
): string | null {
  if (mcp === 'loading') return 'Checking whether the MCP server is on…'
  const missing: string[] = []
  if (mcp === 'off') missing.push('turn on the MCP server')
  if (!keyCreated) missing.push('create a key')
  if (missing.length === 0) return null
  return `First ${missing.join(' and ')}, then ${next}.`
}

/** The logos each path shows, by the id their logo is under. */
export const AGENT_PATH_LOGOS: Record<AgentPath, string[]> = {
  coding_agent: ['claude_cli', 'codex_cli', 'github_copilot', 'cursor'],
  claude_app: ['claude_cli'],
  other_app: ['cursor', 'vscode', 'windsurf', 'zed'],
}
