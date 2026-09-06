// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import { useState, type ReactNode, useId } from 'react'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from './ui/alert-dialog'
import { CopyButton } from './ui/copy-button'
import { Input } from './ui/input'
import { cn } from './lib/cn'
import { Kbd } from './kbd'

/**
 * Every destructive or irreversible action goes through this. Three parts:
 *  1. the verdict: title and one description that names what is lost and
 *     what is kept.
 *  2. typed confirmation: the resource name sits in a mono badge with a copy
 *     button right before the input, so the reader copies it rather than
 *     retyping a 40-character id, and the badge is the thing they are about
 *     to destroy, unmistakably.
 *  3. step progress: the same steps the backend runs, ticked as they finish.
 * Rollback, delete, destroy, revoke, rotate all use it. There is no
 * "are you sure?" dialog anywhere else. `echo` (the equivalent CLI command)
 * is kept for the handoff to the CLI docs but is not rendered.
 *
 * `skin` is the token class to apply to the portal content, since dialogs
 * render outside the `.operator` root.
 */
export function EchoDialog({ trigger, title, description, confirmWord, steps, onDone, destructive, skin = 'operator ink v4 v5', stepMs = 600 }: {
  trigger: ReactNode
  /** Equivalent `temps` CLI command. Documented, not rendered. */
  echo?: string
  title: string
  description: string
  confirmWord: string
  steps: string[]
  onDone: () => void
  destructive?: boolean
  skin?: string
  stepMs?: number
}) {
  const inputId = useId()
  const [typed, setTyped] = useState('')
  const [phase, setPhase] = useState<'idle' | 'running' | 'done'>('idle')
  const [step, setStep] = useState(0)
  const ok = typed === confirmWord
  const run = () => {
    setPhase('running')
    let i = 0
    const id = window.setInterval(() => {
      i += 1
      setStep(i)
      if (i >= steps.length) { window.clearInterval(id); setPhase('done'); onDone() }
    }, stepMs)
  }
  return (
    <AlertDialog onOpenChange={(o) => { if (!o) { setTyped(''); setPhase('idle'); setStep(0) } }}>
      <AlertDialogTrigger asChild>{trigger}</AlertDialogTrigger>
      <AlertDialogContent className={cn(skin, 'gap-0 p-0 sm:rounded')}>
        <div className="space-y-4 p-4">
          <AlertDialogHeader className="space-y-1">
            <AlertDialogTitle className="text-sm font-semibold">{title}</AlertDialogTitle>
            <AlertDialogDescription className="op-prose text-xs">{description}</AlertDialogDescription>
          </AlertDialogHeader>
          {phase === 'idle' ? (
            <div className="space-y-1">
              <label htmlFor={inputId} className="op-label">type the name to confirm</label>
              <div className="flex items-center gap-2">
                {/* The whole badge is the copy button: clicking the name copies it, same as clicking the icon. */}
                <CopyButton value={confirmWord} minimal label={`Copy ${confirmWord}`} title={`Copy ${confirmWord}`} className="h-8 shrink-0 gap-1.5 border bg-muted pl-2 pr-1.5 font-mono text-xs font-normal hover:bg-background [&>svg]:h-3.5 [&>svg]:w-3.5 [&>svg]:text-muted-foreground">
                  <span className="max-w-[40vw] truncate sm:max-w-56">{confirmWord}</span>
                </CopyButton>
                <Input id={inputId} value={typed} onChange={(e) => setTyped(e.target.value)} placeholder={confirmWord} aria-invalid={typed.length > 0 && !ok} className={cn('h-8 min-w-0 flex-1 font-mono text-xs', typed.length > 0 && !ok && 'border-destructive')} autoComplete="off" onKeyDown={(e) => { if (e.key === 'Enter' && ok) { e.preventDefault(); run() } }} />
              </div>
            </div>
          ) : (
            <ol className="op-rows border font-mono text-xs">
              {steps.map((s, i) => (
                <li key={s} className="flex h-7 items-center gap-2 px-2">
                  {/* The running step is marked by glyph and word, never by animation: motion is 100ms
                      transform/shadow/colour only, and a pulsing row reads as a fault. */}
                  <span aria-hidden className={cn('w-3 text-center', i < step ? 'text-success' : i === step && phase === 'running' ? 'text-warning' : 'text-muted-foreground')}>
                    {i < step ? '●' : i === step && phase === 'running' ? '◐' : '○'}
                  </span>
                  <span className={cn(i > step && 'text-muted-foreground')}>{s}</span>
                  <span className="ml-auto text-[10px] text-muted-foreground">{i < step ? 'done' : i === step && phase === 'running' ? 'running' : 'waiting'}</span>
                </li>
              ))}
            </ol>
          )}
          <AlertDialogFooter className="gap-2">
            {phase === 'done' ? (
              <AlertDialogCancel className="h-8 text-xs">close</AlertDialogCancel>
            ) : (
              <>
                <AlertDialogCancel className="h-8 text-xs" disabled={phase === 'running'}>cancel <Kbd keys="esc" className="ml-1 opacity-70" /></AlertDialogCancel>
                <AlertDialogAction
                  disabled={!ok || phase === 'running'}
                  onClick={(e) => { e.preventDefault(); run() }}
                  className={cn('h-8 text-xs disabled:opacity-100', destructive
                    ? 'op-fill-destructive disabled:border-destructive/40 disabled:bg-transparent disabled:text-destructive/60'
                    : 'op-primary disabled:border disabled:border-foreground/40 disabled:bg-transparent disabled:text-foreground/60')}
                >
                  {phase === 'running' ? `step ${Math.min(step + 1, steps.length)} of ${steps.length}` : title.toLowerCase()} {phase === 'idle' && <Kbd keys="⏎" className="ml-1 opacity-70" />}
                </AlertDialogAction>
              </>
            )}
          </AlertDialogFooter>
        </div>
      </AlertDialogContent>
    </AlertDialog>
  )
}
