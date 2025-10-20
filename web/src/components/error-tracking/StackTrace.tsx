/**
 * StackTrace Component
 *
 * Renders a stack trace for error events.
 * Designed to display stack frames as returned by Sentry or Node.js error events.
 *
 * Frame properties:
 *   - filename: string (full path or module)
 *   - function: string (function name)
 *   - lineno: number (line number)
 *   - colno: number (column number)
 *   - context_line: string (source code line, optional)
 *   - pre_context: string[] (lines before context_line, optional)
 *   - post_context: string[] (lines after context_line, optional)
 */

interface StackFrame {
  filename?: string
  function?: string
  lineno?: number
  colno?: number
  context_line?: string
  pre_context?: string[]
  post_context?: string[]
}

interface StackTraceProps {
  frames?: StackFrame[]
  detailed?: boolean
  className?: string
}

export function StackTrace({
  frames,
  detailed = false,
  className = '',
}: StackTraceProps) {
  if (!frames || frames.length === 0) return null

  // Show frames in reverse order (most recent first)
  const reversedFrames = [...frames].reverse()

  return (
    <div
      className={`font-mono text-sm space-y-1 bg-muted/30 p-4 rounded-lg ${className}`}
    >
      {reversedFrames.map((frame, index) => {
        const functionName = frame.function || '<anonymous>'
        const filename = frame.filename || ''
        const lineNo = frame.lineno
        const colNo = frame.colno
        const contextLine = frame.context_line
        const preContext = frame.pre_context
        const postContext = frame.post_context

        // Extract just the filename from the full path
        const shortFilename = filename ? filename.split('/').pop() : 'unknown'

        return (
          <div
            key={index}
            className="group hover:bg-background/50 px-2 py-1 rounded transition-colors"
          >
            <div className="flex items-start gap-2">
              <span className="text-muted-foreground select-none">at</span>
              <div className="flex-1 min-w-0">
                <span
                  className={detailed ? '' : 'hover:underline cursor-pointer'}
                  title={!detailed ? filename : undefined}
                >
                  <span className="text-primary">{functionName}</span>
                  <span className="text-muted-foreground"> (</span>
                  <span className="text-blue-600 dark:text-blue-400">
                    {detailed && filename ? filename : shortFilename}
                    {lineNo !== undefined && `:${lineNo}`}
                    {colNo !== undefined && `:${colNo}`}
                  </span>
                  <span className="text-muted-foreground">)</span>
                </span>
                {(preContext || contextLine || postContext) && (
                  <div className="mt-2 pl-6 space-y-0 text-xs font-mono">
                    {preContext &&
                      preContext.map((line: string, i: number) => (
                        <div
                          key={`pre-${i}`}
                          className="text-muted-foreground/60"
                        >
                          {line}
                        </div>
                      ))}
                    {contextLine && (
                      <div className="bg-yellow-500/10 text-yellow-600 dark:text-yellow-400 px-2 py-0.5 -mx-2">
                        {contextLine}
                      </div>
                    )}
                    {postContext &&
                      postContext.map((line: string, i: number) => (
                        <div
                          key={`post-${i}`}
                          className="text-muted-foreground/60"
                        >
                          {line}
                        </div>
                      ))}
                  </div>
                )}
              </div>
            </div>
          </div>
        )
      })}
    </div>
  )
}
