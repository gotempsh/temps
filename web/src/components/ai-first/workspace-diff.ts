// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

type DiffLineKind = 'added' | 'context' | 'hunk' | 'metadata' | 'removed'

export type ParsedDiffLine = {
  content: string
  kind: DiffLineKind
  newLine: number | null
  oldLine: number | null
}

export type ParsedUnifiedDiff = {
  additions: number
  deletions: number
  isNewFile: boolean
  lines: ParsedDiffLine[]
  omittedLines: number
  totalLines: number
}

const HUNK_HEADER = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)$/

export const MAX_RENDERED_DIFF_LINES = 500

/** Turn a unified Git patch into rows suitable for a code review surface. */
export function parseUnifiedDiff(diff: string): ParsedUnifiedDiff {
  let additions = 0
  let deletions = 0
  let isNewFile = false
  let oldLine: number | null = null
  let newLine: number | null = null
  const lines: ParsedDiffLine[] = []
  let totalLines = 0

  const appendLine = (line: ParsedDiffLine) => {
    totalLines += 1
    if (lines.length < MAX_RENDERED_DIFF_LINES) {
      lines.push(line)
    }
  }

  for (const rawLine of diff.split('\n')) {
    if (rawLine.startsWith('new file mode ')) {
      isNewFile = true
      continue
    }

    if (
      rawLine.startsWith('diff --git ') ||
      rawLine.startsWith('index ') ||
      rawLine.startsWith('deleted file mode ') ||
      rawLine.startsWith('similarity index ') ||
      rawLine.startsWith('rename from ') ||
      rawLine.startsWith('rename to ') ||
      rawLine.startsWith('--- ') ||
      rawLine.startsWith('+++ ')
    ) {
      continue
    }

    const hunk = rawLine.match(HUNK_HEADER)
    if (hunk) {
      oldLine = Number(hunk[1])
      newLine = Number(hunk[2])
      appendLine({
        content: hunk[3]?.trim() || 'Changed lines',
        kind: 'hunk',
        newLine: null,
        oldLine: null,
      })
      continue
    }

    if (rawLine.startsWith('+') && newLine !== null) {
      appendLine({
        content: rawLine.slice(1),
        kind: 'added',
        newLine,
        oldLine: null,
      })
      additions += 1
      newLine += 1
      continue
    }

    if (rawLine.startsWith('-') && oldLine !== null) {
      appendLine({
        content: rawLine.slice(1),
        kind: 'removed',
        newLine: null,
        oldLine,
      })
      deletions += 1
      oldLine += 1
      continue
    }

    if (rawLine.startsWith(' ') && oldLine !== null && newLine !== null) {
      appendLine({
        content: rawLine.slice(1),
        kind: 'context',
        newLine,
        oldLine,
      })
      oldLine += 1
      newLine += 1
      continue
    }

    if (rawLine === '\\ No newline at end of file' || rawLine.length > 0) {
      appendLine({
        content: rawLine,
        kind: 'metadata',
        newLine: null,
        oldLine: null,
      })
    }
  }

  return {
    additions,
    deletions,
    isNewFile,
    lines,
    omittedLines: totalLines - lines.length,
    totalLines,
  }
}
