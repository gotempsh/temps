import { isValidElement, type ReactNode } from 'react'
import { CodeBlock } from '@/components/ui/code-block'
import { codeLanguage } from '@/lib/code-language'

function extractText(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(extractText).join('')
  if (isValidElement<{ children?: ReactNode }>(node))
    return extractText(node.props.children)
  return ''
}

/** React Markdown's pre override; source stays escaped by the shared renderer. */
export function MarkdownCodeBlock({ children }: { children?: ReactNode }) {
  const props = isValidElement<{ className?: string; children?: ReactNode }>(
    children
  )
    ? children.props
    : { children }
  const hint = /language-([\w-]+)/.exec(props.className ?? '')?.[1]
  return (
    <CodeBlock
      code={extractText(props.children).replace(/\n$/, '')}
      language={codeLanguage(hint)}
      className="not-prose my-3 text-left"
    />
  )
}
