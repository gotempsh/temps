// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { describe, expect, test } from 'bun:test'
import { renderToStaticMarkup } from 'react-dom/server'
import { WorkspaceDiffViewer } from './WorkspaceDiffViewer'
import { MAX_RENDERED_DIFF_LINES, parseUnifiedDiff } from './workspace-diff'

const NEW_FILE_DIFF = `diff --git a/projects/demo/config.json b/projects/demo/config.json
new file mode 100644
index 0000000..cf9c65d
--- /dev/null
+++ b/projects/demo/config.json
@@ -0,0 +1,3 @@
+{
+  "strict": true
+}`

describe('WorkspaceDiffViewer', () => {
  test('parses unified diff line numbers and summarizes changes', () => {
    const parsed = parseUnifiedDiff(NEW_FILE_DIFF)

    expect(parsed.isNewFile).toBe(true)
    expect(parsed.additions).toBe(3)
    expect(parsed.deletions).toBe(0)
    expect(parsed.lines[1]).toEqual({
      content: '{',
      kind: 'added',
      newLine: 1,
      oldLine: null,
    })
  })

  test('renders a review surface without raw Git transport headers', () => {
    const html = renderToStaticMarkup(
      <WorkspaceDiffViewer
        diff={NEW_FILE_DIFF}
        path="projects/demo/config.json"
        truncated={false}
      />
    )

    expect(html).toContain('projects/demo/config.json')
    expect(html).toContain('New file')
    expect(html).toContain('+3')
    expect(html).toContain('−0')
    expect(html).toContain('strict')
    expect(html).not.toContain('diff --git')
    expect(html).not.toContain('overflow-x-auto')
  })

  test('keeps old and new counters correct across mixed hunks', () => {
    const parsed = parseUnifiedDiff(
      '@@ -10,3 +10,4 @@ function demo()\n keep\n-remove\n+add\n+another\n keep-two'
    )

    expect(parsed.additions).toBe(2)
    expect(parsed.deletions).toBe(1)
    expect(
      parsed.lines.map(({ oldLine, newLine }) => [oldLine, newLine])
    ).toEqual([
      [null, null],
      [10, 10],
      [11, null],
      [null, 11],
      [null, 12],
      [12, 13],
    ])
  })

  test('caps parsed rows while retaining counts for a pathological large diff', () => {
    const addedLines = Array.from(
      { length: MAX_RENDERED_DIFF_LINES * 4 },
      (_, index) => `+line ${index + 1}`
    )
    const parsed = parseUnifiedDiff(
      `@@ -0,0 +1,${addedLines.length} @@\n${addedLines.join('\n')}`
    )

    expect(parsed.lines).toHaveLength(MAX_RENDERED_DIFF_LINES)
    expect(parsed.additions).toBe(addedLines.length)
    expect(parsed.totalLines).toBe(addedLines.length + 1)
    expect(parsed.omittedLines).toBe(
      addedLines.length + 1 - MAX_RENDERED_DIFF_LINES
    )
  })

  test('renders no more than the line cap and reports omitted line counts', () => {
    const addedLineCount = MAX_RENDERED_DIFF_LINES * 3
    const diff = `@@ -0,0 +1,${addedLineCount} @@\n${Array.from(
      { length: addedLineCount },
      (_, index) => `+bounded line ${index + 1}`
    ).join('\n')}`
    const html = renderToStaticMarkup(
      <WorkspaceDiffViewer diff={diff} path="large.txt" truncated={false} />
    )

    expect(html.match(/data-diff-line="true"/g)).toHaveLength(
      MAX_RENDERED_DIFF_LINES
    )
    expect(html).toContain(
      `Showing the first ${MAX_RENDERED_DIFF_LINES.toLocaleString()} of ${(
        addedLineCount + 1
      ).toLocaleString()} diff lines.`
    )
    expect(html).toContain(
      `${(
        addedLineCount +
        1 -
        MAX_RENDERED_DIFF_LINES
      ).toLocaleString()} lines were not rendered`
    )
    expect(html).not.toContain(`bounded line ${addedLineCount}`)
  })

  test('distinguishes the server byte cap from the client rendering cap', () => {
    const html = renderToStaticMarkup(
      <WorkspaceDiffViewer diff={NEW_FILE_DIFF} path="config.json" truncated />
    )

    expect(html).toContain('server response was truncated at its byte limit')
  })
})
