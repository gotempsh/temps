// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * The transcript of an agent run on Temps' own tool loop (ADR-116 D2): one
 * JSON object per line, `temps.text`, `temps.tool_call`, `temps.tool_result`
 * and `temps.finish`, opened by `temps.truncated` when a long run's earlier
 * lines were left out of the stored transcript (the run's log has them).
 */

export interface TranscriptEvent {
  /** `notice`: something the transcript says about itself, in `content`. */
  type: 'assistant_text' | 'tool_call' | 'tool_result' | 'result' | 'notice'
  content?: string
  tool?: string
  toolUseId?: string
  toolInput?: Record<string, unknown>
  toolResult?: string
  toolStatus?: 'in_progress' | 'completed' | 'failed'
  result?: string
  numTurns?: number
}

function events(output: string): Record<string, unknown>[] {
  const parsed: Record<string, unknown>[] = []
  for (const line of output.split('\n')) {
    const trimmed = line.trim()
    if (!trimmed.startsWith('{')) continue
    try {
      const value: unknown = JSON.parse(trimmed)
      if (value && typeof value === 'object' && !Array.isArray(value)) {
        parsed.push(value as Record<string, unknown>)
      }
    } catch {
      // A line that is not JSON is not part of the transcript.
    }
  }
  return parsed
}

/** Whether `output` is a Temps agent transcript. */
export function isTempsAgentTranscript(output: string): boolean {
  return events(output).some(
    (event) => typeof event.type === 'string' && event.type.startsWith('temps.')
  )
}

/** The run's conversation: what the agent said, the tools it ran and how it ended. */
export function parseTempsAgentTranscript(output: string): TranscriptEvent[] {
  const parsed: TranscriptEvent[] = []
  for (const event of events(output)) {
    switch (event.type) {
      case 'temps.text':
        if (typeof event.text === 'string' && event.text !== '') {
          parsed.push({ type: 'assistant_text', content: event.text })
        }
        break
      case 'temps.tool_call':
        parsed.push({
          type: 'tool_call',
          tool: typeof event.name === 'string' ? event.name : 'tool',
          toolUseId: typeof event.id === 'string' ? event.id : undefined,
          toolInput:
            event.input && typeof event.input === 'object'
              ? (event.input as Record<string, unknown>)
              : {},
          toolStatus: 'in_progress',
        })
        break
      case 'temps.tool_result': {
        const output = typeof event.output === 'string' ? event.output : ''
        const call = parsed.find(
          (e) =>
            e.type === 'tool_call' &&
            e.toolUseId === event.id &&
            e.toolResult === undefined
        )
        const status = event.is_error === true ? 'failed' : 'completed'
        if (call) {
          call.toolResult = output
          call.toolStatus = status
        } else {
          parsed.push({
            type: 'tool_result',
            toolResult: output,
            toolStatus: status,
          })
        }
        break
      }
      case 'temps.truncated': {
        const dropped =
          typeof event.dropped_lines === 'number' ? event.dropped_lines : 0
        parsed.push({
          type: 'notice',
          content: `${dropped} earlier ${dropped === 1 ? 'step is' : 'steps are'} not shown here: the run was long. The run's log has every step.`,
        })
        break
      }
      case 'temps.finish':
        parsed.push({
          type: 'result',
          result:
            event.reason === 'max_turns'
              ? 'The agent used all its turns before finishing.'
              : 'The agent finished.',
          numTurns: typeof event.turns === 'number' ? event.turns : undefined,
        })
        break
    }
  }
  return parsed
}

/** The line a conversation view shows for a run's result or a transcript notice. */
export function resultLine(event: {
  type: string
  content?: string
  numTurns?: number
  costUsd?: number
  tokensInput?: number
}): { label: string; summary: string } {
  if (event.type === 'notice') {
    return { label: 'Not shown', summary: event.content ?? '' }
  }
  return {
    label: 'Result',
    summary: [
      event.numTurns != null ? `${event.numTurns} turns` : null,
      event.costUsd != null ? `$${event.costUsd.toFixed(4)}` : null,
      event.tokensInput != null
        ? `${event.tokensInput.toLocaleString()} in`
        : null,
    ]
      .filter(Boolean)
      .join(' · '),
  }
}
