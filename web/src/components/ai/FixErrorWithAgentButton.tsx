// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { Bot } from 'lucide-react'
import { useNavigate } from 'react-router'
import { Button } from '@/components/ui/button'
import { useAiAssistant } from './AiAssistantContext'
import {
  fixErrorWithAgentOptions,
  type FixableError,
} from './fix-error-with-agent'

/**
 * "Fix with agent" on an error: opens Agents (`/chat`) on a thread about this
 * error in its project. It is shown whether or not AI is set up; without a
 * provider, `/chat` is where that is configured.
 */
export function FixErrorWithAgentButton({
  error,
  className,
}: {
  error: FixableError
  className?: string
}) {
  const navigate = useNavigate()
  const { open } = useAiAssistant()
  return (
    <Button
      type="button"
      variant="outline"
      size="sm"
      className={className}
      title={`Open Agents on error #${error.id} in ${error.project.name}`}
      onClick={(event) => {
        // Rows that hold this button open the error when clicked.
        event.stopPropagation()
        open(fixErrorWithAgentOptions(error))
        navigate('/chat')
      }}
    >
      <Bot aria-hidden="true" className="size-4" />
      <span className="max-sm:sr-only">Fix with agent</span>
    </Button>
  )
}
