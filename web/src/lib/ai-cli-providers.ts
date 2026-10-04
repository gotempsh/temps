// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export const AI_CLI_PROVIDER_SHELL = [
  { id: 'claude_cli', name: 'Claude Code' },
  { id: 'codex_cli', name: 'Codex (OpenAI)' },
  { id: 'opencode', name: 'OpenCode' },
  { id: 'pi', name: 'pi' },
] as const

/**
 * Harnesses that run only inside Temps workspace chat. They are installed in
 * the workspace image and use a relayed workspace credential, so nothing is
 * installed or signed in on the Temps host. The backend rejects them for host
 * execution, project agents, autofix runs, and the instance default.
 */
const WORKSPACE_CHAT_ONLY_PROVIDER_IDS: ReadonlySet<string> = new Set(['pi'])

export function isWorkspaceChatOnlyProvider(providerId: string): boolean {
  return WORKSPACE_CHAT_ONLY_PROVIDER_IDS.has(providerId)
}

/** Providers that can run project agents and autofixes. */
export function projectAgentProviders<T extends { id: string }>(
  providers: readonly T[]
): T[] {
  return providers.filter(
    (provider) => !isWorkspaceChatOnlyProvider(provider.id)
  )
}
