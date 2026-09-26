// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { memo, useMemo } from 'react'
import AnsiToHtml from 'ansi-to-html'

/** Log content is untrusted. Escape markup before applying terminal styles. */
export const AnsiLogMessage = memo(function AnsiLogMessage({
  message,
}: {
  message: string
}) {
  const html = useMemo(
    () =>
      new AnsiToHtml({
        fg: 'var(--foreground)',
        bg: 'var(--background)',
        escapeXML: true,
        newline: false,
        stream: false,
      }).toHtml(message),
    [message]
  )
  return <span dangerouslySetInnerHTML={{ __html: html }} />
})
