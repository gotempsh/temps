// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
import { HAND_OFF_LABEL } from '@/lib/agent-handoff'
import { WandSparkles } from 'lucide-react'
import { CodeBlock, type CodeLanguage } from './code-block'
import { CopyButton } from './copy-button'

export function CopyAiPromptButton({ prompt }: { prompt: string }) {
  return (
    <CopyButton
      value={prompt}
      label={HAND_OFF_LABEL}
      icon={WandSparkles}
      className="h-8 gap-2 rounded-md border px-3 text-xs"
    >
      {HAND_OFF_LABEL}
    </CopyButton>
  )
}

export function AiPromptCodeBlock({
  code,
  language = 'bash',
  prompt,
}: {
  code: string
  language?: CodeLanguage
  prompt: string
}) {
  return (
    <div className="space-y-2">
      <div className="flex justify-end">
        <CopyAiPromptButton prompt={prompt} />
      </div>
      <CodeBlock code={code} language={language} />
    </div>
  )
}
