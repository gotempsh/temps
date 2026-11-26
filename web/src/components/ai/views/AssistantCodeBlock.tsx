// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { ReactNode } from 'react'
import { MarkdownCodeBlock } from '@/components/ui/markdown-code-block'
import { markdownFence } from '@/lib/markdown-fence'
import { AssistantView } from './AssistantView'
import { VIEW_FENCE } from './prompt'

/**
 * The `pre` override for an assistant's answer: a block fenced as a view is
 * drawn, and every other block is the code it always was.
 */
export function AssistantCodeBlock({ children }: { children?: ReactNode }) {
  const fence = markdownFence(children)
  return fence.language === VIEW_FENCE ? (
    <AssistantView source={fence.code} />
  ) : (
    <MarkdownCodeBlock>{children}</MarkdownCodeBlock>
  )
}
