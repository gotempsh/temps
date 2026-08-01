import { ProjectResponse } from '@/api/client'
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import {
  isValidElement,
  useEffect,
  useRef,
  useState,
  type ComponentProps,
  type ReactNode,
} from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import { CodeBlock } from '@/components/ui/code-block'
import {
  AlertTriangle,
  ArrowLeft,
  ExternalLink,
  FileCode,
  Hash,
  Loader2,
  RefreshCw,
  Terminal,
  Webhook,
} from 'lucide-react'
import { Link, useNavigate, useParams } from 'react-router'
import {
  getErrorGroupOptions,
  getRunWithLogsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { AgentRunLogResponse as AgentRunLog } from '@/api/client/types.gen'
import { AutopilotStatusBadge } from './AutopilotStatusBadge'
import { AutofixRunConfigForm } from '@/components/autofixer/AutofixRunConfigForm'
import {
  addContext,
  createPr,
  reAnalyze,
  retryRun,
  startFix,
} from '@/api/client/sdk.gen'
import { cn } from '@/lib/utils'
import { toast } from 'sonner'
import {
  ChevronDown,
  ChevronUp,
  GitBranch,
  MessageSquare,
  Send,
  Sparkles,
} from 'lucide-react'

const proseClasses =
  'prose prose-sm dark:prose-invert max-w-none prose-code:before:content-none prose-code:after:content-none prose-p:my-1.5 prose-headings:my-2 prose-ul:my-1.5 prose-ul:list-disc prose-ul:pl-5 prose-ol:my-1.5 prose-ol:list-decimal prose-ol:pl-5 prose-li:my-0.5 prose-li:marker:text-foreground/60 prose-hr:my-3 prose-hr:border-border prose-table:text-xs prose-th:px-2 prose-th:py-1 prose-td:px-2 prose-td:py-1'

interface AutopilotRunDetailProps {
  project: ProjectResponse
}

const activeStatuses = new Set([
  'pending',
  'cloning',
  'analyzing',
  'fixing',
  'pushing',
  'creating_pr',
  'deploying',
])

/** Autofixer phases that are actively doing work (worth streaming/refetching). */
const autofixerActivePhases = new Set(['analyzing', 'fixing'])

/** Autofixer phases that expose an action (generate fix, create PR, or chat). */
const autofixerInteractivePhases = new Set(['analyzed', 'fix_ready'])

function formatDuration(
  startedAt: string | null | undefined,
  completedAt: string | null | undefined
): string {
  if (!startedAt) return '-'
  const start = new Date(startedAt).getTime()
  const end = completedAt ? new Date(completedAt).getTime() : Date.now()
  const diffSecs = Math.floor((end - start) / 1000)
  if (diffSecs < 60) return `${diffSecs}s`
  const mins = Math.floor(diffSecs / 60)
  const secs = diffSecs % 60
  return `${mins}m ${secs}s`
}

function formatTimestamp(dateStr: string): string {
  return new Date(dateStr).toLocaleTimeString(undefined, {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

function logLevelColor(level: string): string {
  switch (level) {
    case 'error':
      return 'bg-red-500'
    case 'warn':
    case 'warning':
      return 'bg-yellow-500'
    case 'info':
      return 'bg-blue-500'
    case 'debug':
      return 'bg-gray-500'
    default:
      return 'bg-gray-400'
  }
}

type CodeBlockLanguage = NonNullable<
  ComponentProps<typeof CodeBlock>['language']
>

/** Map common markdown fence hints onto the languages the shared CodeBlock
 *  highlights. Unknown/absent hints fall back to 'text' (no highlighting). */
const fenceLanguageMap: Record<string, CodeBlockLanguage> = {
  bash: 'bash',
  sh: 'shell',
  shell: 'shell',
  zsh: 'shell',
  console: 'shell',
  yaml: 'yaml',
  yml: 'yaml',
  json: 'json',
  jsonc: 'json',
  javascript: 'javascript',
  js: 'javascript',
  jsx: 'javascript',
  typescript: 'typescript',
  ts: 'typescript',
  tsx: 'typescript',
  python: 'python',
  py: 'python',
  go: 'go',
  golang: 'go',
  text: 'text',
  txt: 'text',
  plaintext: 'text',
}

function fenceLanguage(className: string | undefined): CodeBlockLanguage {
  const match = /language-([\w-]+)/.exec(className ?? '')
  return (match && fenceLanguageMap[match[1].toLowerCase()]) || 'text'
}

/** Flatten a react-markdown code element's children to the raw code string. */
function extractText(node: ReactNode): string {
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(extractText).join('')
  if (isValidElement(node)) {
    return extractText((node.props as { children?: ReactNode }).children)
  }
  return ''
}

/** Fenced code blocks render through the shared CodeBlock (same component as
 *  the AI Gateway page): syntax colors, line numbers, copy button, and a wrap
 *  toggle. Long lines scroll horizontally instead of stretching the layout. */
function MarkdownPre({ children }: { children?: ReactNode }) {
  if (isValidElement(children)) {
    const codeProps = children.props as {
      className?: string
      children?: ReactNode
    }
    return (
      <CodeBlock
        code={extractText(codeProps.children).replace(/\n$/, '')}
        language={fenceLanguage(codeProps.className)}
        className="not-prose my-3 text-left"
        defaultShowLineNumbers
      />
    )
  }
  return <pre>{children}</pre>
}

/** Render markdown content using prose styles. Fenced code blocks are routed
 *  to the shared CodeBlock via the `pre` component override; inline code keeps
 *  the prose styling (react-markdown only wraps fences in `<pre>`). */
function Markdown({ children }: { children: string }) {
  return (
    <div className={proseClasses}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{ pre: MarkdownPre }}
      >
        {children}
      </ReactMarkdown>
    </div>
  )
}

interface ConversationEvent {
  type: string
  role?: string
  content?: string
  tool?: string
  toolUseId?: string
  toolInput?: Record<string, unknown>
  toolResult?: string
  toolStatus?: 'in_progress' | 'completed' | 'failed'
  result?: string
  numTurns?: number
  costUsd?: number
  durationMs?: number
  tokensInput?: number
  tokensOutput?: number
}

/** Detect whether the output contains Codex-format JSON events.
 *  Codex emits `type: "item.completed"`, `type: "turn.completed"`, etc. */
function isCodexFormat(output: string): boolean {
  for (const line of output.split('\n').slice(0, 10)) {
    const trimmed = line.trim()
    if (!trimmed.startsWith('{')) continue
    if (
      trimmed.includes('"thread.started"') ||
      trimmed.includes('"item.completed"') ||
      trimmed.includes('"turn.completed"') ||
      trimmed.includes('"item.started"')
    ) {
      return true
    }
  }
  return false
}

/** Detect whether the output contains OpenCode-format JSON events.
 *  OpenCode emits `type: "step_start"`, `type: "text"`, `type: "step_finish"`, etc. */
function isOpenCodeFormat(output: string): boolean {
  for (const line of output.split('\n').slice(0, 15)) {
    const trimmed = line.trim()
    if (!trimmed.startsWith('{')) continue

    const hasStepEvent =
      trimmed.includes('"step_start"') ||
      trimmed.includes('"step_finish"') ||
      trimmed.includes('"message.part.updated"')
    const hasTextEventWithPart =
      (trimmed.includes('"type":"text"') ||
        trimmed.includes('"type": "text"')) &&
      trimmed.includes('"part"')

    if (hasStepEvent || hasTextEventWithPart) {
      return true
    }
  }
  return false
}

/** Parse OpenCode CLI --format json output into conversation events.
 *
 *  OpenCode format:
 *  - {"type":"step_start","sessionID":"...","part":{"type":"step-start",...}}
 *  - {"type":"text","sessionID":"...","part":{"type":"text","text":"..."}}
 *  - {"type":"message.part.updated","part":{"type":"tool","name":"bash","state":"running",...}}
 *  - {"type":"message.part.updated","part":{"type":"tool","name":"bash","state":"completed","result":"...",...}}
 *  - {"type":"step_finish","part":{"type":"step-finish","reason":"stop"|"tool-calls","cost":N,"tokens":{...}}}
 */
function parseOpenCodeOutput(output: string): ConversationEvent[] {
  const events: ConversationEvent[] = []
  let totalInputTokens = 0
  let totalOutputTokens = 0
  let totalCost = 0

  for (const line of output.split('\n')) {
    const trimmed = line.trim()
    if (!trimmed || !trimmed.startsWith('{')) continue
    try {
      const parsed = JSON.parse(trimmed)
      const part = parsed.part

      if (parsed.type === 'text' && part?.type === 'text' && part.text) {
        // Assistant text — accumulate consecutive text events
        const lastEvent = events[events.length - 1]
        if (lastEvent?.type === 'assistant_text') {
          lastEvent.content = (lastEvent.content || '') + part.text
        } else {
          events.push({ type: 'assistant_text', content: part.text })
        }
      } else if (
        parsed.type === 'message.part.updated' &&
        part?.type === 'tool'
      ) {
        const toolName = part.name || 'unknown'
        // Capitalize first letter to match Claude/Codex rendering (bash → Bash)
        const displayName = toolName.charAt(0).toUpperCase() + toolName.slice(1)

        if (part.state === 'running' || part.state === 'pending') {
          // Tool started
          events.push({
            type: 'tool_call',
            tool: displayName,
            toolUseId: part.id,
            toolInput: part.input ?? (part.args ? JSON.parse(part.args) : {}),
            toolStatus: 'in_progress',
          })
        } else if (part.state === 'completed' || part.state === 'error') {
          // Tool finished — merge into existing in_progress event or create new
          const existing = part.id
            ? events.find(
                (e) => e.type === 'tool_call' && e.toolUseId === part.id
              )
            : [...events]
                .reverse()
                .find(
                  (e) =>
                    e.type === 'tool_call' &&
                    e.tool === displayName &&
                    e.toolStatus === 'in_progress'
                )

          const resultText =
            typeof part.result === 'string' ? part.result : (part.output ?? '')

          if (existing) {
            existing.toolResult = resultText
            existing.toolStatus =
              part.state === 'error' ? 'failed' : 'completed'
            // Backfill input if the started event didn't have it
            if (
              !existing.toolInput ||
              Object.keys(existing.toolInput).length === 0
            ) {
              existing.toolInput =
                part.input ?? (part.args ? JSON.parse(part.args) : {})
            }
          } else {
            events.push({
              type: 'tool_call',
              tool: displayName,
              toolUseId: part.id,
              toolInput: part.input ?? (part.args ? JSON.parse(part.args) : {}),
              toolResult: resultText,
              toolStatus: part.state === 'error' ? 'failed' : 'completed',
            })
          }
        }
      } else if (
        parsed.type === 'step_finish' &&
        part?.type === 'step-finish'
      ) {
        // Accumulate tokens and cost from each step
        if (part.tokens) {
          totalInputTokens += part.tokens.input || 0
          totalOutputTokens += part.tokens.output || 0
        }
        if (part.cost != null) {
          totalCost += part.cost
        }
      }
    } catch {
      // Not valid JSON
    }
  }

  // Add a synthetic result event with accumulated usage
  if (
    events.length > 0 &&
    (totalInputTokens > 0 || totalOutputTokens > 0 || totalCost > 0)
  ) {
    events.push({
      type: 'result',
      tokensInput: totalInputTokens,
      tokensOutput: totalOutputTokens,
      costUsd: totalCost > 0 ? totalCost : undefined,
    })
  }

  return events
}

/** Parse Codex CLI --json output into conversation events.
 *
 *  Codex format:
 *  - {"type":"item.completed","item":{"type":"agent_message","text":"..."}}
 *  - {"type":"item.started","item":{"type":"command_execution","command":"...","status":"in_progress"}}
 *  - {"type":"item.completed","item":{"type":"command_execution","command":"...","aggregated_output":"...","exit_code":0,"status":"completed"}}
 *  - {"type":"turn.completed","usage":{"input_tokens":N,"output_tokens":N}}
 */
function parseCodexOutput(output: string): ConversationEvent[] {
  const events: ConversationEvent[] = []
  let totalInputTokens = 0
  let totalOutputTokens = 0

  for (const line of output.split('\n')) {
    const trimmed = line.trim()
    if (!trimmed || !trimmed.startsWith('{')) continue
    try {
      const parsed = JSON.parse(trimmed)
      const item = parsed.item

      if (
        parsed.type === 'item.completed' &&
        item?.type === 'agent_message' &&
        item.text
      ) {
        events.push({ type: 'assistant_text', content: item.text })
      } else if (
        parsed.type === 'item.started' &&
        item?.type === 'command_execution'
      ) {
        // Command started — create a tool_call event that will be merged
        // with the completed event when it arrives
        events.push({
          type: 'tool_call',
          tool: 'Bash',
          toolUseId: item.id,
          toolInput: { command: item.command },
          toolStatus: 'in_progress',
        })
      } else if (
        parsed.type === 'item.completed' &&
        item?.type === 'command_execution'
      ) {
        // Command completed — merge into existing tool_call or create new one
        const existing = item.id
          ? events.find(
              (e) => e.type === 'tool_call' && e.toolUseId === item.id
            )
          : [...events]
              .reverse()
              .find(
                (e) =>
                  e.type === 'tool_call' &&
                  !e.toolResult &&
                  e.toolInput?.command === item.command
              )

        const resultText = item.aggregated_output || ''
        const exitCode = item.exit_code ?? null
        const statusLabel =
          item.status === 'failed' || (exitCode != null && exitCode !== 0)
            ? `[exit ${exitCode}] `
            : ''
        const fullResult = `${statusLabel}${resultText}`

        if (existing) {
          existing.toolResult = fullResult
          existing.toolStatus =
            item.status === 'failed' ? 'failed' : 'completed'
        } else {
          events.push({
            type: 'tool_call',
            tool: 'Bash',
            toolUseId: item.id,
            toolInput: { command: item.command },
            toolResult: fullResult,
            toolStatus: item.status === 'failed' ? 'failed' : 'completed',
          })
        }
      } else if (
        parsed.type === 'item.completed' &&
        item?.type === 'function_call'
      ) {
        // Codex function calls (MCP tools, etc.)
        events.push({
          type: 'tool_call',
          tool: item.name || 'function',
          toolUseId: item.id,
          toolInput: item.arguments ? JSON.parse(item.arguments) : {},
          toolResult: item.output || '',
          toolStatus: 'completed',
        })
      } else if (parsed.type === 'turn.completed' && parsed.usage) {
        totalInputTokens += parsed.usage.input_tokens || 0
        totalOutputTokens += parsed.usage.output_tokens || 0
      }
    } catch {
      // Not valid JSON
    }
  }

  // Add a synthetic result event with accumulated token usage
  if (events.length > 0 && (totalInputTokens > 0 || totalOutputTokens > 0)) {
    events.push({
      type: 'result',
      tokensInput: totalInputTokens,
      tokensOutput: totalOutputTokens,
    })
  }

  return events
}

/** Parse Claude stream-json output into conversation events.
 *  Merges consecutive tool_call → tool_result pairs so the result
 *  is available on the tool_call event itself (rendered collapsed). */
function parseClaudeOutput(output: string): ConversationEvent[] {
  const events: ConversationEvent[] = []

  for (const line of output.split('\n')) {
    const trimmed = line.trim()
    if (!trimmed || !trimmed.startsWith('{')) continue
    try {
      const parsed = JSON.parse(trimmed)

      if (parsed.type === 'assistant' && parsed.message?.content) {
        for (const block of parsed.message.content) {
          if (block.type === 'text' && block.text) {
            events.push({ type: 'assistant_text', content: block.text })
          } else if (block.type === 'tool_use') {
            events.push({
              type: 'tool_call',
              tool: block.name,
              toolUseId: block.id,
              toolInput: block.input,
            })
          }
        }
      } else if (parsed.type === 'user' && parsed.message?.content) {
        for (const block of parsed.message.content) {
          if (block.type === 'tool_result') {
            const content = Array.isArray(block.content)
              ? block.content
                  .map((c: { text?: string }) => c.text || '')
                  .join('')
              : typeof block.content === 'string'
                ? block.content
                : JSON.stringify(block.content)
            const matchById = block.tool_use_id
              ? events.find(
                  (e) =>
                    e.type === 'tool_call' &&
                    e.toolUseId === block.tool_use_id &&
                    !e.toolResult
                )
              : null
            const target =
              matchById ||
              [...events]
                .reverse()
                .find((e) => e.type === 'tool_call' && !e.toolResult)
            if (target) {
              target.toolResult = content
            } else {
              events.push({ type: 'tool_result', toolResult: content })
            }
          }
        }
      } else if (parsed.type === 'tool_result') {
        const content = Array.isArray(parsed.content)
          ? parsed.content.map((c: { text?: string }) => c.text || '').join('')
          : typeof parsed.content === 'string'
            ? parsed.content
            : JSON.stringify(parsed.content)
        const prev = events.length > 0 ? events[events.length - 1] : null
        if (prev && prev.type === 'tool_call' && !prev.toolResult) {
          prev.toolResult = content
        } else {
          events.push({ type: 'tool_result', toolResult: content })
        }
      } else if (parsed.type === 'result') {
        events.push({
          type: 'result',
          result: parsed.result,
          numTurns: parsed.num_turns,
          costUsd: parsed.total_cost_usd,
          durationMs: parsed.duration_ms,
        })
      }
    } catch {
      // Not JSON
    }
  }

  return events
}

/** Parse stream output from any AI provider into conversation events.
 *  Auto-detects OpenCode vs Codex vs Claude format. */
function parseStreamOutput(output: string): ConversationEvent[] {
  if (isOpenCodeFormat(output)) return parseOpenCodeOutput(output)
  if (isCodexFormat(output)) return parseCodexOutput(output)
  return parseClaudeOutput(output)
}

/** Claude-managed-agent style viewer: Transcript + Debug with inspector pane. */
function ConversationViewer({
  output,
  systemPrompt,
  live,
}: {
  output: string
  systemPrompt?: string | null
  live?: boolean
}) {
  const events = parseStreamOutput(output)
  const [mode, setMode] = useState<'transcript' | 'debug'>('transcript')
  const [filter, setFilter] = useState<
    'all' | 'user' | 'assistant' | 'tool' | 'system'
  >('all')
  const [selectedIdx, setSelectedIdx] = useState<number | null>(null)
  const listRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (live && listRef.current) {
      listRef.current.scrollTop = listRef.current.scrollHeight
    }
  }, [events.length, live])

  // Build a unified event list with a synthetic "system" entry for the prompt.
  type ViewEvent = {
    kind: 'system' | 'user' | 'assistant' | 'tool' | 'result'
    label: string
    summary: string
    raw: unknown
    tool?: string
    status?: string
  }
  const viewEvents: ViewEvent[] = []
  if (systemPrompt) {
    viewEvents.push({
      kind: 'system',
      label: 'System prompt',
      summary: systemPrompt.slice(0, 160),
      raw: systemPrompt,
    })
  }
  for (const e of events) {
    if (e.type === 'assistant_text' && e.content) {
      viewEvents.push({
        kind: 'assistant',
        label: 'Assistant',
        summary: e.content.slice(0, 200),
        raw: e,
      })
    } else if (e.type === 'tool_call') {
      const input = e.toolInput || {}
      const preview =
        (input.file_path as string) ||
        (input.command as string) ||
        (input.pattern as string) ||
        JSON.stringify(input).slice(0, 160)
      viewEvents.push({
        kind: 'tool',
        label: e.tool || 'Tool',
        summary: preview,
        tool: e.tool,
        status: e.toolStatus,
        raw: e,
      })
    } else if (e.type === 'tool_result' && e.toolResult) {
      viewEvents.push({
        kind: 'tool',
        label: 'Tool result',
        summary: e.toolResult.slice(0, 200),
        raw: e,
      })
    } else if (e.type === 'result') {
      viewEvents.push({
        kind: 'result',
        label: 'Result',
        summary: [
          e.numTurns != null ? `${e.numTurns} turns` : null,
          e.costUsd != null ? `$${e.costUsd.toFixed(4)}` : null,
          e.tokensInput != null ? `${e.tokensInput.toLocaleString()} in` : null,
        ]
          .filter(Boolean)
          .join(' · '),
        raw: e,
      })
    }
  }

  const filtered = viewEvents.filter((v) => {
    if (filter === 'all') return true
    if (filter === 'user') return v.kind === 'user'
    if (filter === 'assistant') return v.kind === 'assistant'
    if (filter === 'tool') return v.kind === 'tool'
    if (filter === 'system') return v.kind === 'system' || v.kind === 'result'
    return true
  })

  const selected =
    selectedIdx != null && filtered[selectedIdx] ? filtered[selectedIdx] : null

  const kindStyle = (kind: ViewEvent['kind']): string => {
    switch (kind) {
      case 'system':
        return 'bg-muted text-muted-foreground border-border'
      case 'user':
        return 'bg-pink-500/10 text-pink-600 dark:text-pink-300 border-pink-500/20'
      case 'assistant':
        return 'bg-purple-500/10 text-purple-600 dark:text-purple-300 border-purple-500/20'
      case 'tool':
        return 'bg-blue-500/10 text-blue-600 dark:text-blue-300 border-blue-500/20'
      case 'result':
        return 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-300 border-emerald-500/20'
    }
  }

  if (events.length === 0 && !systemPrompt) {
    return (
      <Card>
        <CardContent className="p-6 text-center text-sm text-muted-foreground">
          {live ? 'Waiting for events…' : 'No conversation data.'}
        </CardContent>
      </Card>
    )
  }

  return (
    <div className="flex flex-col -mx-1">
      {/* Compact toolbar — no card, tight spacing */}
      <div className="flex items-center gap-2 flex-wrap px-1 py-1.5 text-xs">
        <button
          onClick={() => setMode('transcript')}
          className={cn(
            'px-2 py-0.5 rounded transition',
            mode === 'transcript'
              ? 'bg-foreground/10 text-foreground font-medium'
              : 'text-muted-foreground hover:text-foreground'
          )}
        >
          Transcript
        </button>
        <button
          onClick={() => setMode('debug')}
          className={cn(
            'px-2 py-0.5 rounded transition',
            mode === 'debug'
              ? 'bg-foreground/10 text-foreground font-medium'
              : 'text-muted-foreground hover:text-foreground'
          )}
        >
          Debug
        </button>
        <span className="text-border">·</span>
        <select
          value={filter}
          onChange={(e) => setFilter(e.target.value as typeof filter)}
          className="bg-transparent text-muted-foreground hover:text-foreground cursor-pointer px-1 py-0.5"
        >
          <option value="all">All ({viewEvents.length})</option>
          <option value="assistant">
            Assistant ({viewEvents.filter((v) => v.kind === 'assistant').length}
            )
          </option>
          <option value="tool">
            Tool ({viewEvents.filter((v) => v.kind === 'tool').length})
          </option>
          <option value="system">
            System (
            {
              viewEvents.filter(
                (v) => v.kind === 'system' || v.kind === 'result'
              ).length
            }
            )
          </option>
        </select>
        <div className="flex-1" />
        {live && (
          <span className="text-[10px] font-medium text-emerald-500 animate-pulse tracking-wide">
            ● LIVE
          </span>
        )}
      </div>

      {/* Main — two pane on desktop, single column on mobile */}
      <div className="grid grid-cols-1 lg:grid-cols-[minmax(0,1fr)_minmax(0,22rem)] border-t border-border">
        {/* Event list */}
        <div
          ref={listRef}
          className="overflow-y-auto border-b lg:border-b-0 lg:border-r border-border max-h-[calc(100vh-16rem)]"
        >
          {mode === 'transcript' ? (
            <div className="p-4 space-y-4">
              {filtered.map((v, i) => {
                if (v.kind === 'system') {
                  return (
                    <details
                      key={i}
                      className="rounded-md border border-border bg-muted/20"
                    >
                      <summary className="px-3 py-2 text-xs font-medium text-muted-foreground cursor-pointer select-none">
                        System prompt
                      </summary>
                      <div className="px-3 py-2 text-sm text-muted-foreground">
                        <Markdown>{systemPrompt || ''}</Markdown>
                      </div>
                    </details>
                  )
                }
                if (v.kind === 'assistant') {
                  return (
                    <div key={i} className="flex gap-3">
                      <span
                        className={cn(
                          'shrink-0 h-6 px-2 text-[10px] font-medium rounded border uppercase tracking-wide flex items-center',
                          kindStyle('assistant')
                        )}
                      >
                        Assistant
                      </span>
                      <div className="text-sm min-w-0 flex-1">
                        <Markdown>
                          {(v.raw as ConversationEvent).content || ''}
                        </Markdown>
                      </div>
                    </div>
                  )
                }
                if (v.kind === 'tool') {
                  const e = v.raw as ConversationEvent
                  const resultText = e.toolResult || ''
                  const isFailed = e.toolStatus === 'failed'
                  return (
                    <div
                      key={i}
                      className={cn(
                        'rounded-md border',
                        isFailed
                          ? 'border-red-500/20 bg-red-500/5'
                          : 'border-blue-500/15 bg-blue-500/5'
                      )}
                    >
                      <div className="flex items-center gap-2 px-3 py-2">
                        <span
                          className={cn(
                            'text-xs font-mono font-medium',
                            isFailed ? 'text-red-500' : 'text-blue-500'
                          )}
                        >
                          {e.tool}
                        </span>
                        <span className="text-xs font-mono text-muted-foreground truncate flex-1">
                          {v.summary}
                        </span>
                        {e.toolStatus === 'in_progress' && (
                          <Loader2 className="h-3 w-3 animate-spin text-blue-500" />
                        )}
                        {isFailed && (
                          <span className="text-[10px] font-medium text-red-500">
                            FAILED
                          </span>
                        )}
                      </div>
                      {resultText.length > 2 && (
                        <details className="border-t border-border/40">
                          <summary className="px-3 py-1.5 text-[11px] text-muted-foreground cursor-pointer select-none hover:bg-muted/40">
                            Output (
                            {resultText.length > 1000
                              ? `${Math.round(resultText.length / 1000)}k`
                              : resultText.length}{' '}
                            chars)
                          </summary>
                          <pre className="text-xs font-mono bg-muted/30 px-3 py-2 max-h-48 overflow-auto whitespace-pre-wrap break-words text-muted-foreground">
                            {resultText.slice(0, 2000)}
                            {resultText.length > 2000 ? '\n…' : ''}
                          </pre>
                        </details>
                      )}
                    </div>
                  )
                }
                if (v.kind === 'result') {
                  return (
                    <div
                      key={i}
                      className={cn(
                        'rounded-md border px-3 py-2 text-xs',
                        kindStyle('result')
                      )}
                    >
                      <span className="font-medium mr-2">Result</span>
                      <span className="tabular-nums">{v.summary}</span>
                    </div>
                  )
                }
                return null
              })}
              {filtered.length === 0 && (
                <p className="text-xs text-muted-foreground text-center py-8">
                  No events match the filter.
                </p>
              )}
            </div>
          ) : (
            /* Debug list — compact row per event, click to inspect */
            <ul className="divide-y divide-border">
              {filtered.map((v, i) => {
                const isSelected = selectedIdx === i
                return (
                  <li key={i}>
                    <button
                      onClick={() => setSelectedIdx(isSelected ? null : i)}
                      className={cn(
                        'w-full flex items-center gap-3 px-3 py-2 text-left hover:bg-muted/40 transition',
                        isSelected && 'bg-muted/60'
                      )}
                    >
                      <span
                        className={cn(
                          'shrink-0 h-5 px-1.5 text-[10px] font-medium rounded border uppercase tracking-wide flex items-center',
                          kindStyle(v.kind)
                        )}
                      >
                        {v.kind}
                      </span>
                      <span className="text-sm font-medium shrink-0">
                        {v.label}
                      </span>
                      <span className="text-xs text-muted-foreground truncate flex-1 font-mono">
                        {v.summary}
                      </span>
                    </button>
                  </li>
                )
              })}
              {filtered.length === 0 && (
                <li className="px-3 py-8 text-xs text-muted-foreground text-center">
                  No events match the filter.
                </li>
              )}
            </ul>
          )}
        </div>

        {/* Inspector pane — only shown in debug mode and when something is selected */}
        <aside
          className={cn(
            'bg-muted/10',
            mode !== 'debug' || !selected ? 'hidden' : 'block'
          )}
        >
          {selected && (
            <div className="p-4 space-y-3 max-h-[600px] overflow-y-auto">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <span
                    className={cn(
                      'shrink-0 h-5 px-1.5 text-[10px] font-medium rounded border uppercase tracking-wide flex items-center',
                      kindStyle(selected.kind)
                    )}
                  >
                    {selected.kind}
                  </span>
                  <span className="text-sm font-semibold">
                    {selected.label}
                  </span>
                </div>
                <button
                  onClick={() => setSelectedIdx(null)}
                  className="text-muted-foreground hover:text-foreground text-xs"
                  aria-label="Close inspector"
                >
                  ✕
                </button>
              </div>
              {selected.kind === 'system' &&
              typeof selected.raw === 'string' ? (
                <>
                  <p className="text-[10px] font-mono text-muted-foreground uppercase tracking-wide">
                    prompt
                  </p>
                  <div className="text-sm text-muted-foreground bg-background border border-border rounded p-3 max-h-96 overflow-auto">
                    <Markdown>{selected.raw}</Markdown>
                  </div>
                </>
              ) : selected.kind === 'assistant' &&
                (selected.raw as ConversationEvent).content ? (
                <>
                  <p className="text-[10px] font-mono text-muted-foreground uppercase tracking-wide">
                    message
                  </p>
                  <div className="text-sm text-muted-foreground bg-background border border-border rounded p-3 max-h-96 overflow-auto">
                    <Markdown>
                      {(selected.raw as ConversationEvent).content || ''}
                    </Markdown>
                  </div>
                </>
              ) : (
                <>
                  <p className="text-[10px] font-mono text-muted-foreground uppercase tracking-wide">
                    raw
                  </p>
                  <pre className="text-[11px] font-mono bg-background border border-border rounded p-3 overflow-auto whitespace-pre-wrap break-words text-muted-foreground">
                    {typeof selected.raw === 'string'
                      ? selected.raw
                      : JSON.stringify(selected.raw, null, 2)}
                  </pre>
                </>
              )}
            </div>
          )}
        </aside>
      </div>
    </div>
  )
}

export function AutopilotRunDetail({ project }: AutopilotRunDetailProps) {
  const { runId } = useParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const [streamLogs, setStreamLogs] = useState<AgentRunLog[]>([])
  const [isStreaming, setIsStreaming] = useState(false)
  const [isRetrying, setIsRetrying] = useState(false)
  const [contextInput, setContextInput] = useState('')
  const [isSendingContext, setIsSendingContext] = useState(false)
  const [isStartingFix, setIsStartingFix] = useState(false)
  const [isCreatingPr, setIsCreatingPr] = useState(false)
  const [showFeedback, setShowFeedback] = useState(false)
  // Retry/start-over config dialog: prefilled from the run's stored config
  // so the user can change harness/model/turns/branch and add retry notes.
  const [showRetryConfig, setShowRetryConfig] = useState(false)

  const runQueryOptions = getRunWithLogsOptions({
    path: { project_id: project.id, run_id: Number(runId) },
  })

  const { data, isLoading, error } = useQuery({
    ...runQueryOptions,
    enabled: !!runId,
    refetchInterval: (query) => {
      const run = query.state.data?.run
      if (!run) return false
      const phase = run.phase || ''
      if (activeStatuses.has(run.status)) return 5000
      if (autofixerActivePhases.has(phase)) return 5000
      return false
    },
  })

  const currentRun = data?.run
  const isAutofixerRun = currentRun?.trigger_source_type === 'error_group'
  const errorGroupId = isAutofixerRun
    ? (currentRun?.trigger_source_id ?? null)
    : null

  const { data: errorGroup } = useQuery({
    ...getErrorGroupOptions({
      path: {
        project_id: project.id,
        group_id: Number(errorGroupId),
      },
    }),
    enabled: !!errorGroupId,
  })

  // SSE real-time streaming
  useEffect(() => {
    if (!runId || !data?.run) return
    const phase = data.run.phase || ''
    const shouldStream =
      activeStatuses.has(data.run.status) || autofixerActivePhases.has(phase)
    if (!shouldStream) return

    setIsStreaming(true)
    // Autofixer runs have their own SSE endpoint; regular runs use the generic one.
    const streamUrl =
      data.run.trigger_source_type === 'error_group'
        ? `/api/projects/${project.id}/autofixer/runs/${runId}/stream`
        : `/api/projects/${project.id}/agents/runs/${runId}/stream`
    const eventSource = new EventSource(streamUrl)

    eventSource.onmessage = (event) => {
      try {
        const log = JSON.parse(event.data) as AgentRunLog
        setStreamLogs((prev) => {
          // Dedup by id
          if (prev.some((l) => l.id === log.id)) return prev
          return [...prev, log]
        })
      } catch {
        // Not JSON
      }
    }

    eventSource.addEventListener('status', () => {
      // Run completed / phase changed — close stream and refetch
      eventSource.close()
      setIsStreaming(false)
      queryClient.invalidateQueries({ queryKey: runQueryOptions.queryKey })
    })

    eventSource.onerror = () => {
      eventSource.close()
      setIsStreaming(false)
    }

    return () => {
      eventSource.close()
      setIsStreaming(false)
    }
  }, [
    runId,
    data?.run?.status,
    data?.run?.phase,
    data?.run?.trigger_source_type,
    project.id,
    queryClient,
    runQueryOptions.queryKey,
  ])

  if (isLoading) {
    return (
      <div className="space-y-4">
        <Skeleton className="h-8 w-48" />
        <Skeleton className="h-40 w-full" />
        <Skeleton className="h-64 w-full" />
      </div>
    )
  }

  if (error || !data) {
    return (
      <Alert variant="destructive">
        <AlertTriangle className="h-4 w-4" />
        <AlertTitle>Error</AlertTitle>
        <AlertDescription>
          {error instanceof Error ? error.message : 'Failed to load run'}
        </AlertDescription>
      </Alert>
    )
  }

  const { run, logs: fetchedLogs } = data

  // Merge fetched logs with streamed logs, dedup by id
  const allLogs = (() => {
    const merged = [...fetchedLogs]
    for (const sl of streamLogs) {
      if (!merged.some((l) => l.id === sl.id)) {
        merged.push(sl)
      }
    }
    return merged.sort((a, b) => a.id - b.id)
  })()

  const logs = allLogs

  const phase = run.phase || ''
  const isAutofixer = run.trigger_source_type === 'error_group'
  const isAutofixerActive = isAutofixer && autofixerActivePhases.has(phase)
  const isAutofixerWaiting =
    isAutofixer && autofixerInteractivePhases.has(phase)
  const isActive = activeStatuses.has(run.status) || isAutofixerActive
  // Autofixer runs in interactive phases (analyzed / fix_ready) are NOT "done" —
  // a retry would throw away the user's analysis work. Only offer Retry when
  // the run is terminal (completed/failed/cancelled).
  const canRetry =
    !isActive && !isAutofixerWaiting && run.source !== 'cli_ephemeral'

  const onRetry = async () => {
    if (isAutofixer && run.trigger_source_id) {
      // Autofixer retry = configure + start a fresh analysis on the same
      // error group. The dialog prefills this run's stored config.
      setShowRetryConfig(true)
      return
    }
    setIsRetrying(true)
    try {
      const { data: newRun } = await retryRun({
        path: { project_id: project.id, run_id: run.id },
        throwOnError: true,
      })
      navigate(`../agents/${newRun.id}`)
    } catch (e) {
      console.error('Failed to retry run:', e)
      toast.error(e instanceof Error ? e.message : 'Retry failed')
    } finally {
      setIsRetrying(false)
    }
  }

  const onStartFix = async () => {
    setIsStartingFix(true)
    try {
      await startFix({
        path: { project_id: project.id, run_id: run.id },
        throwOnError: true,
      })
      setStreamLogs([])
      queryClient.invalidateQueries({ queryKey: runQueryOptions.queryKey })
      toast.success('Fix generation started')
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'Failed to start fix')
    } finally {
      setIsStartingFix(false)
    }
  }

  const onCreatePr = async () => {
    setIsCreatingPr(true)
    try {
      await createPr({
        path: { project_id: project.id, run_id: run.id },
        throwOnError: true,
      })
      queryClient.invalidateQueries({ queryKey: runQueryOptions.queryKey })
      toast.success('Pull request created')
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'Failed to create PR')
    } finally {
      setIsCreatingPr(false)
    }
  }

  const onSendContext = async () => {
    const message = contextInput.trim()
    if (!message || isSendingContext) return
    setContextInput('')
    setIsSendingContext(true)
    try {
      await addContext({
        path: { project_id: project.id, run_id: run.id },
        body: { message },
        throwOnError: true,
      })
      if (phase === 'analyzed') {
        await reAnalyze({
          path: { project_id: project.id, run_id: run.id },
          throwOnError: true,
        })
      }
      queryClient.invalidateQueries({ queryKey: runQueryOptions.queryKey })
    } catch (e) {
      toast.error(e instanceof Error ? e.message : 'Failed to send message')
    } finally {
      setIsSendingContext(false)
    }
  }

  // "Start Over" — for autofixer runs only. Opens the run-config dialog
  // (prefilled from this run) and kicks off a fresh analysis on submit.
  const onStartOver = () => {
    if (!run.trigger_source_id) return
    setShowRetryConfig(true)
  }

  const onCancel = async () => {
    try {
      await fetch(`/api/projects/${project.id}/agents/runs/${run.id}/cancel`, {
        method: 'POST',
      })
      window.location.reload()
    } catch {
      // ignore
    }
  }

  const durationText = formatDuration(run.started_at, run.completed_at)
  const costText =
    run.estimated_cost_cents != null
      ? `$${(run.estimated_cost_cents / 100).toFixed(2)}`
      : null
  const providerLabel =
    run.ai_provider === 'claude_cli'
      ? 'Claude Code'
      : run.ai_provider === 'codex_cli'
        ? 'Codex'
        : run.ai_provider === 'opencode'
          ? 'OpenCode'
          : run.ai_provider

  // Split logs into AI conversation events and system logs for tab views.
  const aiEvents = logs.filter((l: AgentRunLog) => l.level === 'ai_event')
  const systemLogs = logs.filter((l: AgentRunLog) => l.level !== 'ai_event')
  const combinedAiOutput = run.ai_output
    ? run.ai_output
    : aiEvents.length > 0
      ? aiEvents.map((l: AgentRunLog) => l.message).join('\n')
      : ''
  const showLiveAi = !run.ai_output && aiEvents.length > 0

  const PrimaryActions = (
    <>
      {run.preview_url && (
        <Button size="sm" asChild>
          <a href={run.preview_url} target="_blank" rel="noopener noreferrer">
            <ExternalLink className="h-3.5 w-3.5 mr-1.5" />
            Preview
          </a>
        </Button>
      )}
      {run.pr_url && (
        <Button size="sm" variant="outline" asChild>
          <a href={run.pr_url} target="_blank" rel="noopener noreferrer">
            <Hash className="h-3.5 w-3.5 mr-1.5" />
            PR #{run.pr_number}
          </a>
        </Button>
      )}
      {canRetry && (
        <Button
          size="sm"
          variant="ghost"
          disabled={isRetrying}
          onClick={onRetry}
        >
          {isRetrying ? (
            <Loader2 className="h-3.5 w-3.5 animate-spin mr-1.5" />
          ) : (
            <RefreshCw className="h-3.5 w-3.5 mr-1.5" />
          )}
          {isAutofixer ? 'Start over' : 'Retry'}
        </Button>
      )}
      {isActive && (
        <Button size="sm" variant="destructive" onClick={onCancel}>
          Cancel
        </Button>
      )}
    </>
  )

  // ── Autofixer phase action bar + chat ─────────────────────────────────
  const autofixerActionBar = isAutofixer ? (
    <>
      {(phase === 'analyzed' ||
        phase === 'fix_ready' ||
        phase === 'completed' ||
        phase === 'no_fix' ||
        phase === 'pr_created' ||
        run.status === 'failed' ||
        run.status === 'cancelled') && (
        <Card
          className={cn(
            phase === 'completed' || phase === 'pr_created'
              ? 'border-green-500/20 bg-green-500/5'
              : run.status === 'failed' || run.status === 'cancelled'
                ? 'border-red-500/20 bg-red-500/5'
                : 'border-blue-500/20 bg-blue-500/5'
          )}
        >
          <CardContent className="p-4 flex items-center justify-between gap-4 flex-wrap">
            <div className="text-sm min-w-0">
              {phase === 'analyzed' &&
                'Analysis complete. Generate a fix, or open feedback to refine.'}
              {phase === 'fix_ready' &&
                'Fix is ready. Review the changes, then create a pull request.'}
              {(phase === 'completed' || phase === 'pr_created') &&
                run.pr_url && (
                  <span className="flex items-center gap-2">
                    Pull request created —
                    <a
                      href={run.pr_url}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="text-green-400 hover:underline font-medium inline-flex items-center gap-1"
                    >
                      View PR #{run.pr_number}{' '}
                      <ExternalLink className="h-3 w-3" />
                    </a>
                  </span>
                )}
              {phase === 'no_fix' && (
                <span className="text-muted-foreground">
                  No automatic fix available. Send feedback to retry.
                </span>
              )}
              {(run.status === 'failed' || run.status === 'cancelled') && (
                <span className="text-red-400">
                  {run.status === 'failed' ? 'Run failed' : 'Run cancelled'}
                </span>
              )}
            </div>
            <div className="flex gap-2 flex-shrink-0">
              {(['completed', 'failed', 'cancelled'] as const).includes(
                run.status as 'completed' | 'failed' | 'cancelled'
              ) && (
                <Button variant="outline" size="sm" onClick={onStartOver}>
                  <RefreshCw className="h-4 w-4 mr-2" />
                  Start Over
                </Button>
              )}
              {(phase === 'analyzing' || phase === 'analyzed') && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => setShowFeedback((v) => !v)}
                  aria-expanded={showFeedback}
                >
                  <MessageSquare className="h-4 w-4 mr-2" />
                  Feedback
                  {showFeedback ? (
                    <ChevronUp className="h-4 w-4 ml-2" />
                  ) : (
                    <ChevronDown className="h-4 w-4 ml-2" />
                  )}
                </Button>
              )}
              {phase === 'analyzed' && (
                <Button onClick={onStartFix} disabled={isStartingFix} size="sm">
                  {isStartingFix ? (
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  ) : (
                    <Sparkles className="h-4 w-4 mr-2" />
                  )}
                  Generate Fix
                </Button>
              )}
              {phase === 'fix_ready' && (
                <Button onClick={onCreatePr} disabled={isCreatingPr} size="sm">
                  {isCreatingPr ? (
                    <Loader2 className="h-4 w-4 mr-2 animate-spin" />
                  ) : (
                    <GitBranch className="h-4 w-4 mr-2" />
                  )}
                  Create PR
                </Button>
              )}
            </div>
          </CardContent>
        </Card>
      )}

      {(phase === 'analyzing' || phase === 'analyzed') && showFeedback && (
        <div className="flex gap-2 items-end">
          <textarea
            placeholder={
              phase === 'analyzed'
                ? "Send feedback to re-analyze — e.g. 'Check the auth middleware'"
                : "Add context — e.g. 'This started after the auth migration'"
            }
            value={contextInput}
            onChange={(e) => {
              setContextInput(e.target.value)
              e.target.style.height = 'auto'
              e.target.style.height = e.target.scrollHeight + 'px'
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault()
                onSendContext()
              }
            }}
            rows={1}
            className="flex-1 rounded-md border border-border bg-transparent px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-primary resize-none overflow-hidden"
            disabled={isSendingContext}
          />
          <Button
            size="sm"
            onClick={onSendContext}
            disabled={!contextInput.trim() || isSendingContext}
          >
            {isSendingContext ? (
              <Loader2 className="h-4 w-4 animate-spin" />
            ) : (
              <Send className="h-4 w-4" />
            )}
          </Button>
        </div>
      )}
    </>
  ) : null

  // ── Tab contents ──────────────────────────────────────────────────────
  const ConversationTab =
    combinedAiOutput || run.prompt_text ? (
      <ConversationViewer
        output={combinedAiOutput}
        systemPrompt={run.prompt_text}
        live={showLiveAi && isStreaming}
      />
    ) : (
      <Card>
        <CardContent className="p-6 text-center text-sm text-muted-foreground">
          {isActive
            ? 'Waiting for the agent to start the conversation…'
            : 'No conversation recorded for this run.'}
        </CardContent>
      </Card>
    )

  const ReportTab = run.analysis ? (
    <Card>
      <CardContent className="p-5 text-sm">
        <Markdown>{run.analysis}</Markdown>
      </CardContent>
    </Card>
  ) : (
    <Card>
      <CardContent className="p-6 text-center text-sm text-muted-foreground">
        No report produced for this run.
      </CardContent>
    </Card>
  )

  const LogsTab =
    systemLogs.length > 0 ? (
      <Card>
        <CardContent className="p-4 space-y-2">
          {systemLogs.map((log: AgentRunLog) => (
            <div key={log.id} className="flex items-start gap-3">
              <div className="flex flex-col items-center pt-1.5">
                <div
                  className={cn(
                    'h-2 w-2 rounded-full flex-shrink-0',
                    logLevelColor(log.level)
                  )}
                />
              </div>
              <div className="min-w-0 flex-1">
                <div className="flex items-center gap-2">
                  <span className="text-xs text-muted-foreground whitespace-nowrap">
                    {formatTimestamp(log.created_at)}
                  </span>
                  <span className="text-xs font-medium uppercase text-muted-foreground">
                    {log.level}
                  </span>
                </div>
                <p className="text-sm break-words">{log.message}</p>
              </div>
            </div>
          ))}
        </CardContent>
      </Card>
    ) : (
      <Card>
        <CardContent className="p-6 text-center text-sm text-muted-foreground">
          No system logs recorded.
        </CardContent>
      </Card>
    )

  const DetailsTab = (
    <div className="space-y-4">
      {isAutofixer && errorGroup && (
        <Card className="border-red-500/20 bg-red-500/5">
          <CardHeader className="pb-3">
            <div className="flex items-center gap-2">
              <AlertTriangle className="h-4 w-4 text-red-500" />
              <CardTitle className="text-sm">
                Error group{' '}
                <Link
                  to={`../errors/${errorGroup.id}`}
                  className="hover:underline inline-flex items-center gap-1"
                >
                  #{errorGroup.id}
                  <ExternalLink className="h-3 w-3" />
                </Link>
              </CardTitle>
            </div>
          </CardHeader>
          <CardContent className="pt-0 space-y-2">
            <p className="text-sm font-medium leading-snug break-words">
              {errorGroup.title}
            </p>
            <div className="flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
              {errorGroup.error_type && (
                <span>
                  <span className="text-muted-foreground/70">Type:</span>{' '}
                  <code className="text-xs">{errorGroup.error_type}</code>
                </span>
              )}
              {errorGroup.total_count != null && (
                <span>
                  <span className="text-muted-foreground/70">Occurrences:</span>{' '}
                  {errorGroup.total_count}
                </span>
              )}
              {errorGroup.first_seen && (
                <span>
                  <span className="text-muted-foreground/70">First seen:</span>{' '}
                  {new Date(errorGroup.first_seen).toLocaleString()}
                </span>
              )}
            </div>
          </CardContent>
        </Card>
      )}
      {run.error_message && (
        <Alert variant="destructive">
          <AlertTriangle className="h-4 w-4" />
          <AlertTitle className="flex items-center justify-between gap-2">
            Error
            {run.error_message.length > 1200 && (
              <Dialog>
                <DialogTrigger asChild>
                  <Button variant="outline" size="sm">
                    View full
                  </Button>
                </DialogTrigger>
                <DialogContent className="max-w-3xl">
                  <DialogHeader>
                    <DialogTitle>Full error message</DialogTitle>
                  </DialogHeader>
                  <pre className="text-xs bg-muted/40 border border-border rounded p-3 overflow-auto max-h-[70vh] whitespace-pre-wrap break-words">
                    {run.error_message}
                  </pre>
                </DialogContent>
              </Dialog>
            )}
          </AlertTitle>
          <AlertDescription className="whitespace-pre-wrap font-mono text-xs max-h-48 overflow-y-auto break-words">
            {run.error_message.length > 1200
              ? run.error_message.slice(0, 1200) + '\n\n… (truncated)'
              : run.error_message}
          </AlertDescription>
        </Alert>
      )}
      {run.source === 'cli_ephemeral' && (
        <Alert>
          <FileCode className="h-4 w-4" />
          <AlertTitle className="flex items-center gap-2">
            Ephemeral CLI run
            <span className="text-[10px] uppercase tracking-wide bg-blue-500/10 text-blue-400 px-1.5 py-0.5 rounded font-medium">
              dry-run
            </span>
          </AlertTitle>
          <AlertDescription className="space-y-2">
            <p>
              Triggered with{' '}
              <code className="text-xs bg-muted px-1 py-0.5 rounded">
                temps workflow run --from-file
              </code>
              . Workflow config lives only on this run.
            </p>
            {run.ephemeral_yaml && (
              <Dialog>
                <DialogTrigger asChild>
                  <Button variant="outline" size="sm">
                    <FileCode className="h-3 w-3 mr-1.5" />
                    View YAML
                  </Button>
                </DialogTrigger>
                <DialogContent className="max-w-3xl">
                  <DialogHeader>
                    <DialogTitle>Workflow YAML</DialogTitle>
                    <DialogDescription>
                      The exact config the executor ran.
                    </DialogDescription>
                  </DialogHeader>
                  <pre className="text-xs bg-muted/40 border border-border rounded p-3 overflow-auto max-h-[60vh] whitespace-pre-wrap break-all">
                    {run.ephemeral_yaml}
                  </pre>
                </DialogContent>
              </Dialog>
            )}
          </AlertDescription>
        </Alert>
      )}
      {(run.trigger_type || run.user_context) && (
        <Card>
          <CardHeader className="pb-3">
            <div className="flex items-center gap-2">
              {run.trigger_type === 'webhook' ? (
                <Webhook className="h-4 w-4 text-muted-foreground" />
              ) : (
                <Terminal className="h-4 w-4 text-muted-foreground" />
              )}
              <CardTitle className="text-sm">
                Trigger: <span className="capitalize">{run.trigger_type}</span>
              </CardTitle>
            </div>
          </CardHeader>
          {run.user_context && (
            <CardContent className="pt-0">
              <pre className="whitespace-pre-wrap text-xs font-mono bg-muted p-3 rounded-md overflow-x-auto max-h-48 overflow-y-auto">
                {(() => {
                  try {
                    return JSON.stringify(JSON.parse(run.user_context), null, 2)
                  } catch {
                    return run.user_context
                  }
                })()}
              </pre>
            </CardContent>
          )}
        </Card>
      )}
      {run.prompt_text && (
        <Card>
          <CardHeader className="pb-3">
            <div className="flex items-center justify-between gap-2">
              <div className="flex items-center gap-2">
                <FileCode className="h-4 w-4 text-muted-foreground" />
                <CardTitle className="text-sm">Prompt sent to AI</CardTitle>
              </div>
              <Dialog>
                <DialogTrigger asChild>
                  <Button variant="outline" size="sm">
                    <FileCode className="h-3 w-3 mr-1.5" />
                    View full
                  </Button>
                </DialogTrigger>
                <DialogContent className="max-w-3xl">
                  <DialogHeader>
                    <DialogTitle>Final assembled prompt</DialogTitle>
                    <DialogDescription>
                      Trigger context, error details, YAML prompt, and user
                      context merged.
                    </DialogDescription>
                  </DialogHeader>
                  <pre className="text-xs bg-muted/40 border border-border rounded p-3 overflow-auto max-h-[70vh] whitespace-pre-wrap break-words">
                    {run.prompt_text}
                  </pre>
                </DialogContent>
              </Dialog>
            </div>
          </CardHeader>
          <CardContent className="pt-0">
            <pre className="whitespace-pre-wrap text-xs font-mono bg-muted p-3 rounded-md overflow-x-auto max-h-48 overflow-y-auto">
              {run.prompt_text.length > 1200
                ? run.prompt_text.slice(0, 1200) + '\n\n… (truncated)'
                : run.prompt_text}
            </pre>
          </CardContent>
        </Card>
      )}
      <dl className="grid grid-cols-2 sm:grid-cols-3 gap-x-4 gap-y-3 text-xs px-1">
        {run.branch_name && (
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Branch</dt>
            <dd className="font-mono truncate" title={run.branch_name}>
              {run.branch_name}
            </dd>
          </div>
        )}
        {providerLabel && (
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Provider</dt>
            <dd>{providerLabel}</dd>
          </div>
        )}
        {run.ai_model && (
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Model</dt>
            <dd className="font-mono truncate" title={run.ai_model}>
              {run.ai_model}
            </dd>
          </div>
        )}
        {run.tokens_input != null && (
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Tokens</dt>
            <dd className="tabular-nums">
              {run.tokens_input.toLocaleString()} /{' '}
              {(run.tokens_output ?? 0).toLocaleString()}
            </dd>
          </div>
        )}
        {run.ai_session_id && (
          <div className="space-y-0.5">
            <dt className="text-muted-foreground">Session</dt>
            <dd className="font-mono truncate" title={run.ai_session_id}>
              {run.ai_session_id.slice(0, 8)}…
            </dd>
          </div>
        )}
      </dl>
    </div>
  )

  return (
    <div className="space-y-6 pb-24 lg:pb-6">
      <div className="space-y-2">
        {/* Single compact row: back · run id · status · agent name · inline metrics · actions */}
        <div className="flex items-center gap-2 flex-wrap">
          <Button
            variant="ghost"
            size="icon"
            asChild
            className="shrink-0 -ml-2 h-7 w-7"
          >
            <Link to="../agents">
              <ArrowLeft className="h-4 w-4" />
            </Link>
          </Button>
          <span className="text-xs text-muted-foreground font-mono shrink-0">
            #{run.id}
          </span>
          <AutopilotStatusBadge status={run.status} />
          {run.agent_name && (
            <h1 className="text-sm font-semibold truncate min-w-0">
              {run.agent_name}
            </h1>
          )}
          {isStreaming && (
            <span className="text-[10px] font-medium text-emerald-500 animate-pulse tracking-wide">
              ● LIVE
            </span>
          )}
          <span className="hidden sm:inline text-xs text-muted-foreground tabular-nums ml-auto">
            {run.files_changed ?? 0} files · {durationText}
            {costText ? ` · ${costText}` : ''}
          </span>
          <div className="flex items-center gap-1 shrink-0 ml-auto sm:ml-2">
            {PrimaryActions}
          </div>
        </div>

        {autofixerActionBar && (
          <div className="space-y-2">{autofixerActionBar}</div>
        )}

        {/* Tabs as plain text links, no pill, no card */}
        <Tabs defaultValue="conversation" className="w-full">
          <TabsList className="h-auto bg-transparent p-0 gap-4 justify-start border-b border-border w-full rounded-none">
            <TabsTrigger
              value="conversation"
              className="data-[state=active]:bg-transparent data-[state=active]:shadow-none data-[state=active]:border-b-2 data-[state=active]:border-foreground rounded-none px-0 pb-2 text-sm font-medium text-muted-foreground data-[state=active]:text-foreground"
            >
              Conversation
            </TabsTrigger>
            <TabsTrigger
              value="report"
              className="data-[state=active]:bg-transparent data-[state=active]:shadow-none data-[state=active]:border-b-2 data-[state=active]:border-foreground rounded-none px-0 pb-2 text-sm font-medium text-muted-foreground data-[state=active]:text-foreground"
            >
              Report
            </TabsTrigger>
            <TabsTrigger
              value="details"
              className="data-[state=active]:bg-transparent data-[state=active]:shadow-none data-[state=active]:border-b-2 data-[state=active]:border-foreground rounded-none px-0 pb-2 text-sm font-medium text-muted-foreground data-[state=active]:text-foreground"
            >
              Details
            </TabsTrigger>
            <TabsTrigger
              value="logs"
              className="data-[state=active]:bg-transparent data-[state=active]:shadow-none data-[state=active]:border-b-2 data-[state=active]:border-foreground rounded-none px-0 pb-2 text-sm font-medium text-muted-foreground data-[state=active]:text-foreground"
            >
              Logs
            </TabsTrigger>
          </TabsList>
          <TabsContent value="conversation" className="mt-2">
            {ConversationTab}
          </TabsContent>
          <TabsContent value="report" className="mt-4">
            {ReportTab}
          </TabsContent>
          <TabsContent value="details" className="mt-4">
            {DetailsTab}
          </TabsContent>
          <TabsContent value="logs" className="mt-4">
            {LogsTab}
          </TabsContent>
        </Tabs>
      </div>

      {/* Retry / start-over config dialog (autofixer runs). Prefilled from
          this run's stored config; notes become the new run's user context. */}
      <Dialog open={showRetryConfig} onOpenChange={setShowRetryConfig}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>Start a new autofix run</DialogTitle>
            <DialogDescription>
              Adjust the harness, model, turn limit, or branch — and tell the
              model what to do differently this time.
            </DialogDescription>
          </DialogHeader>
          {run.trigger_source_id != null && (
            <AutofixRunConfigForm
              project={project}
              errorGroupId={run.trigger_source_id}
              initialConfig={run.run_config}
              notesPlaceholder="Notes for this retry — e.g. what went wrong last time, or where to focus..."
              submitLabel="Start New Run"
              onStarted={(newRunId) => {
                setShowRetryConfig(false)
                navigate(`../agents/${newRunId}`)
              }}
              onCancel={() => setShowRetryConfig(false)}
            />
          )}
        </DialogContent>
      </Dialog>
    </div>
  )
}
