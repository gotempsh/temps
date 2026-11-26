// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { CodeBlock } from '@/components/ui/code-block'
import { codeLanguage } from '@/lib/code-language'
import { markdownFence } from '@/lib/markdown-fence'

/** React Markdown's pre override; source stays escaped by the shared renderer. */
export function MarkdownCodeBlock({ children }: { children?: ReactNode }) {
  const fence = markdownFence(children)
  return (
    <CodeBlock
      code={fence.code}
      language={codeLanguage(fence.language)}
      className="not-prose my-3 text-left"
    />
  )
}
