// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Projects a service can still be linked to, split into the ones this chat
 * made (offered first: almost always the one meant) and the rest. Projects it
 * is already linked to are left out.
 */
export function linkCandidates<T extends { id: number }>(
  projects: readonly T[],
  linkedProjectIds: readonly number[],
  chatProjectIds: readonly number[]
): { chat: T[]; other: T[] } {
  const linked = new Set(linkedProjectIds)
  const fromChat = new Set(chatProjectIds)
  const open = projects.filter((project) => !linked.has(project.id))
  return {
    chat: open.filter((project) => fromChat.has(project.id)),
    other: open.filter((project) => !fromChat.has(project.id)),
  }
}
