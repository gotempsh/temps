// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { cn } from '@/lib/utils'
import { HelpCircle } from 'lucide-react'
import type { AskUserOutcome } from './ask-user'

/**
 * A question the assistant asked during a turn and what the user answered,
 * as saved on the tool call. The live question card is where it is answered;
 * this is the record that stays in the transcript.
 */
export function AskUserAnswerCard({ outcome }: { outcome: AskUserOutcome }) {
  const answered = outcome.status === 'answered'
  return (
    <div
      className={cn(
        'min-w-0 overflow-hidden rounded-lg border text-xs',
        answered
          ? 'border-blue-500/30 bg-blue-500/5'
          : 'border-border bg-muted/30'
      )}
    >
      <div className="flex items-center gap-2 px-2.5 py-1.5">
        <HelpCircle
          className={cn(
            'h-3.5 w-3.5 shrink-0',
            answered
              ? 'text-blue-600 dark:text-blue-400'
              : 'text-muted-foreground'
          )}
          aria-hidden="true"
        />
        <span className="text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">
          {answered ? 'Your answers' : 'Question not answered'}
        </span>
      </div>
      {outcome.status === 'answered' ? (
        <dl className="space-y-2 border-t border-blue-500/20 px-2.5 py-2">
          {outcome.answers.map((item) => (
            <div key={item.question} className="min-w-0 space-y-0.5">
              <dt className="text-muted-foreground">
                {item.header && (
                  <span className="mr-1.5 font-medium text-foreground/70">
                    {item.header}
                  </span>
                )}
                {item.question}
              </dt>
              <dd
                className={cn(
                  'whitespace-pre-wrap break-words',
                  item.answer
                    ? 'font-medium text-foreground'
                    : 'italic text-muted-foreground'
                )}
              >
                {item.answer || 'Left blank'}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        outcome.reason && (
          <p className="border-t px-2.5 py-2 text-muted-foreground">
            The assistant asked a question, but {outcome.reason}.
          </p>
        )
      )}
    </div>
  )
}
