// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { JSONUIProvider, Renderer } from '@json-render/react'
import { Component, useMemo, type ReactNode } from 'react'
import { CodeBlock } from '@/components/ui/code-block'
import { Skeleton } from '@/components/ui/skeleton'
import { viewRegistry } from './registry'
import { useViewStreaming } from './view-streaming'
import { parseViewSpec } from './view-spec'

/** Shows `fallback` if drawing throws, and tries again when the view changes. */
class ViewBoundary extends Component<
  { source: string; fallback: ReactNode; children: ReactNode },
  { source: string; failed: boolean }
> {
  state = { source: this.props.source, failed: false }

  static getDerivedStateFromError() {
    return { failed: true }
  }

  static getDerivedStateFromProps(
    props: { source: string },
    state: { source: string }
  ) {
    return props.source === state.source
      ? null
      : { source: props.source, failed: false }
  }

  render() {
    return this.state.failed ? this.props.fallback : this.props.children
  }
}

/**
 * A view the assistant wrote in a fenced block, drawn with the console's
 * components. While the answer streams it fills in as each line arrives.
 *
 * A block that describes nothing drawable is never hidden: once the answer
 * has settled it is shown as the code it is, so what the model wrote can
 * still be read.
 */
export function AssistantView({ source }: { source: string }) {
  const streaming = useViewStreaming()
  const view = useMemo(() => parseViewSpec(source), [source])
  const written = (
    <CodeBlock
      code={source}
      language="json"
      className="not-prose my-3 text-left"
    />
  )

  if (!view) {
    return streaming ? (
      <div
        className="not-prose my-3 space-y-2"
        role="status"
        aria-label="Drawing the view"
      >
        <Skeleton className="h-9 w-full" />
        <Skeleton className="h-9 w-full" />
      </div>
    ) : (
      written
    )
  }

  return (
    <ViewBoundary source={source} fallback={written}>
      <div className="not-prose my-3 min-w-0" data-assistant-view="">
        <JSONUIProvider registry={viewRegistry}>
          <Renderer
            spec={view.spec}
            registry={viewRegistry}
            loading={streaming}
          />
        </JSONUIProvider>
        {/* Never a quietly shorter view: what was left out is said, and
            what the model wrote stays one click away. */}
        {!streaming && view.dropped > 0 ? (
          <details className="mt-2 text-xs text-muted-foreground">
            <summary className="cursor-pointer select-none hover:text-foreground">
              Part of this view could not be drawn. Show what was written
            </summary>
            {written}
          </details>
        ) : null}
      </div>
    </ViewBoundary>
  )
}
