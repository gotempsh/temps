// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

const HARNESS_NAMES: Record<string, string> = {
  claude_cli: 'Claude Code',
  codex_cli: 'Codex',
  opencode: 'OpenCode',
  // pi's own name is lowercase.
  pi: 'pi',
}

export function canonicalHarnessId(providerId: string): string {
  const normalized = providerId.trim().toLowerCase()
  // "pi" occurs inside ordinary words ("api", "pipeline", "spinner"), so it
  // is only ever recognized as the exact provider id, never as a substring.
  if (normalized === 'pi') return 'pi'
  if (normalized.includes('claude') || normalized.includes('anthropic')) {
    return 'claude_cli'
  }
  if (normalized.includes('codex') || normalized === 'openai') {
    return 'codex_cli'
  }
  if (normalized.includes('opencode')) return 'opencode'
  return normalized
}

export function aiHarnessName(providerId: string): string {
  return HARNESS_NAMES[canonicalHarnessId(providerId)] ?? providerId
}
