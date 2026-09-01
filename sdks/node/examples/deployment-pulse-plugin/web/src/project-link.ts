// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

export function projectPath(slug: string): string {
  return `/projects/${encodeURIComponent(slug)}`;
}
