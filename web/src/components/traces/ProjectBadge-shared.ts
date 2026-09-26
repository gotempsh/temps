// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Deterministic per-project colour so a project keeps the same hue across the
// legend, its span badges, and its waterfall rows. Shared by the single-project
// trace detail (`TraceDetail`) and the standalone unified view
// (`CrossProjectTraceDetail`) so a project looks identical in both.
export const PROJECT_COLORS = [
  '#6366f1', // indigo
  '#10b981', // emerald
  '#f59e0b', // amber
  '#ec4899', // pink
  '#06b6d4', // cyan
  '#8b5cf6', // violet
  '#ef4444', // red
  '#14b8a6', // teal
  '#0ea5e9', // sky
  '#a855f7', // purple
]

export function projectColor(projectId: number): string {
  return PROJECT_COLORS[Math.abs(projectId) % PROJECT_COLORS.length]
}
