// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import {
  AlertTriangle,
  ArrowRight,
  ArrowUp,
  Check,
  CheckCircle2,
  ChevronDown,
  Circle,
  Cloud,
  Code2,
  Copy,
  CreditCard,
  Database,
  EyeOff,
  GitBranch,
  Globe2,
  KeyRound,
  LoaderCircle,
  Lock,
  LockKeyhole,
  MoreHorizontal,
  Network,
  PanelLeft,
  Pencil,
  Plus,
  RotateCcw,
  Server,
  Settings2,
  ShieldCheck,
  Sparkles,
  Wand2,
  X,
  Zap,
} from 'lucide-react'
import {
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
  type FormEvent,
} from 'react'
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
} from '@/components/ui/alert-dialog'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { usePageTitle } from '@/hooks/usePageTitle'
import { writeToClipboard } from '@/lib/clipboard'
import {
  buildSecretReferencePayload,
  containsLikelyCredential,
  type SecretDraft,
  type SecretReference,
} from '@/lib/ai-first-security'
import { cn } from '@/lib/utils'
import { AiFirstWorkspace } from '@/components/ai-first/AiFirstWorkspace'

const SECRET_REQUIREMENTS = [
  {
    key: 'STRIPE_SECRET_KEY',
    description: 'Stripe payments',
    targets: ['payments-api'],
  },
  {
    key: 'RESEND_API_KEY',
    description: 'Transactional email',
    targets: ['storefront-web', 'fulfillment-worker'],
  },
] as const

const conversations = [
  { title: 'Launch commerce suite', detail: '2 minutes ago', active: true },
  { title: 'Database recovery plan', detail: 'Yesterday', active: false },
  { title: 'Why did checkout fail?', detail: 'Tuesday', active: false },
] as const

const DEFAULT_CONVERSATION_TITLE = conversations[0].title

const palette = {
  '--ai-canvas': '#0b0d0c',
  '--ai-panel': '#111411',
  '--ai-panel-raised': '#171b17',
  '--ai-line': '#2a302a',
  '--ai-line-soft': '#202520',
  '--ai-text': '#f0f3e9',
  '--ai-muted': '#98a092',
  '--ai-lime': '#d7ff63',
  '--ai-lime-ink': '#192000',
  '--ai-amber': '#ffc665',
} as CSSProperties

type DeploymentPhase = 'review' | 'applying' | 'live'

interface ExtraExchange {
  id: number
  prompt: string
  response: string
}

export function AiFirstPrototypePreview() {
  usePageTitle('AI-first prototype')
  const [composer, setComposer] = useState('')
  const [secretDialogOpen, setSecretDialogOpen] = useState(false)
  const [secretReferences, setSecretReferences] = useState<SecretReference[]>(
    []
  )
  const [securityNotice, setSecurityNotice] = useState<string | null>(null)
  const [phase, setPhase] = useState<DeploymentPhase>('review')
  const [applyStep, setApplyStep] = useState(0)
  const [exchanges, setExchanges] = useState<ExtraExchange[]>([])
  const [railOpen, setRailOpen] = useState(false)
  const [conversationTitle, setConversationTitle] = useState<string>(
    DEFAULT_CONVERSATION_TITLE
  )
  const [renameDialogOpen, setRenameDialogOpen] = useState(false)
  const [renameDraft, setRenameDraft] = useState<string>(
    DEFAULT_CONVERSATION_TITLE
  )
  const [conversationNotice, setConversationNotice] = useState<string | null>(
    null
  )

  useEffect(() => {
    if (phase !== 'applying') return

    const stepOne = window.setTimeout(() => setApplyStep(1), 500)
    const stepTwo = window.setTimeout(() => setApplyStep(2), 1_150)
    const complete = window.setTimeout(() => {
      setApplyStep(3)
      setPhase('live')
    }, 1_900)

    return () => {
      window.clearTimeout(stepOne)
      window.clearTimeout(stepTwo)
      window.clearTimeout(complete)
    }
  }, [phase])

  useEffect(() => {
    if (!conversationNotice) return
    const timeout = window.setTimeout(() => setConversationNotice(null), 2_500)
    return () => window.clearTimeout(timeout)
  }, [conversationNotice])

  const missingSecrets = useMemo(
    () =>
      SECRET_REQUIREMENTS.filter(
        (requirement) =>
          !secretReferences.some((secret) => secret.key === requirement.key)
      ),
    [secretReferences]
  )
  const secretsReady = missingSecrets.length === 0

  const submitPrompt = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const prompt = composer.trim()
    if (!prompt) return

    if (containsLikelyCredential(prompt)) {
      setSecurityNotice(
        'I stopped that message because it looks like it contains a credential. Add it through the secure secret broker instead.'
      )
      setComposer('')
      setSecretDialogOpen(true)
      return
    }

    setExchanges((current) => [
      ...current,
      {
        id: Date.now(),
        prompt,
        response:
          'Understood. I added that as a constraint to the proposed plan. Nothing has been changed yet.',
      },
    ])
    setComposer('')
  }

  const storeSecrets = (drafts: SecretDraft[]) => {
    const nextReferences = buildSecretReferencePayload('commerce-suite', drafts)
    setSecretReferences((current) => {
      const retained = current.filter(
        (saved) =>
          !nextReferences.some((incoming) => incoming.key === saved.key)
      )
      return [...retained, ...nextReferences]
    })
    setSecurityNotice(null)
  }

  const applyPlan = () => {
    if (!secretsReady || phase !== 'review') return
    setApplyStep(0)
    setPhase('applying')
  }

  const openRenameDialog = () => {
    setRenameDraft(conversationTitle)
    setRenameDialogOpen(true)
  }

  const renameConversation = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const nextTitle = renameDraft.trim()
    if (!nextTitle) return
    setConversationTitle(nextTitle)
    setRenameDialogOpen(false)
    setConversationNotice('Conversation renamed')
  }

  const copyConversationLink = async () => {
    try {
      await writeToClipboard(window.location.href)
      setConversationNotice('Conversation link copied')
    } catch {
      setConversationNotice('Your browser blocked clipboard access')
    }
  }

  const resetPrototype = () => {
    setComposer('')
    setSecretDialogOpen(false)
    setSecretReferences([])
    setSecurityNotice(null)
    setPhase('review')
    setApplyStep(0)
    setExchanges([])
    setConversationTitle(DEFAULT_CONVERSATION_TITLE)
    setConversationNotice('Prototype reset')
  }

  return (
    <div
      className="fixed inset-0 z-40 overflow-hidden bg-[var(--ai-canvas)] text-[var(--ai-text)]"
      style={palette}
    >
      <div
        aria-hidden="true"
        className="pointer-events-none absolute inset-0 opacity-60"
        style={{
          backgroundImage:
            'radial-gradient(circle at 58% -10%, rgba(215,255,99,0.09), transparent 34%), linear-gradient(rgba(255,255,255,0.018) 1px, transparent 1px), linear-gradient(90deg, rgba(255,255,255,0.018) 1px, transparent 1px)',
          backgroundSize: 'auto, 32px 32px, 32px 32px',
        }}
      />

      <header className="relative z-10 flex h-14 items-center justify-between border-b border-[var(--ai-line)] bg-[color-mix(in_srgb,var(--ai-canvas)_88%,transparent)] px-3 backdrop-blur-xl sm:px-5">
        <div className="flex items-center gap-3">
          <button
            type="button"
            className="flex size-8 items-center justify-center rounded-lg border border-[var(--ai-line)] text-[var(--ai-muted)] transition-colors hover:text-[var(--ai-text)] lg:hidden"
            onClick={() => setRailOpen((open) => !open)}
            aria-label="Toggle conversations"
          >
            <PanelLeft className="size-4" />
          </button>
          <div className="flex size-7 items-center justify-center rounded-lg bg-[var(--ai-lime)] text-[var(--ai-lime-ink)] shadow-[0_0_24px_rgba(215,255,99,0.18)]">
            <span className="text-sm font-black tracking-[-0.08em]">T</span>
          </div>
          <div className="flex items-baseline gap-2">
            <span className="text-sm font-semibold tracking-tight">Temps</span>
            <span className="hidden text-xs text-[var(--ai-muted)] sm:inline">
              Operator
            </span>
          </div>
          <div className="hidden items-center gap-1.5 rounded-full border border-[var(--ai-line)] bg-[var(--ai-panel)] px-2.5 py-1 text-[10px] font-medium uppercase tracking-[0.14em] text-[var(--ai-muted)] sm:flex">
            <Circle className="size-1.5 fill-[var(--ai-lime)] text-[var(--ai-lime)]" />
            Prototype · no live changes
          </div>
        </div>
        <div className="flex items-center gap-2">
          <div className="hidden items-center gap-2 rounded-lg border border-[var(--ai-line)] bg-[var(--ai-panel)] px-3 py-1.5 text-xs text-[var(--ai-muted)] md:flex">
            <ShieldCheck className="size-3.5 text-[var(--ai-lime)]" />
            Guarded mode
            <ChevronDown className="size-3" />
          </div>
          <Button
            asChild
            variant="ghost"
            size="sm"
            className="h-8 text-[var(--ai-muted)] hover:bg-[var(--ai-panel-raised)] hover:text-[var(--ai-text)]"
          >
            <a href="/projects">
              <X className="size-4 sm:mr-1.5" />
              <span className="hidden sm:inline">Classic console</span>
            </a>
          </Button>
        </div>
      </header>

      <div className="relative z-10 grid h-[calc(100dvh-3.5rem)] min-h-0 grid-cols-1 lg:grid-cols-[230px_minmax(0,1fr)] xl:grid-cols-[230px_minmax(560px,1fr)_330px]">
        <ConversationRail
          open={railOpen}
          activeTitle={conversationTitle}
          onClose={() => setRailOpen(false)}
        />

        <main className="relative flex min-h-0 min-w-0 flex-col bg-[color-mix(in_srgb,var(--ai-canvas)_80%,transparent)]">
          <div className="border-b border-[var(--ai-line-soft)] px-4 py-3 sm:px-6">
            <div className="mx-auto flex max-w-3xl items-center justify-between gap-4">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <h1 className="truncate text-sm font-medium">
                    {conversationTitle}
                  </h1>
                  <span className="rounded-full bg-[rgba(215,255,99,0.09)] px-2 py-0.5 text-[10px] font-medium text-[var(--ai-lime)]">
                    onboarding
                  </span>
                </div>
                {conversationNotice ? (
                  <p
                    className="mt-0.5 truncate text-xs text-[var(--ai-lime)]"
                    role="status"
                  >
                    {conversationNotice}
                  </p>
                ) : (
                  <p className="mt-0.5 truncate text-xs text-[var(--ai-muted)]">
                    AI generates the interface needed for each decision
                  </p>
                )}
              </div>
              <DropdownMenu>
                <DropdownMenuTrigger asChild>
                  <button
                    type="button"
                    className="rounded-md p-1.5 text-[var(--ai-muted)] transition-colors hover:bg-[var(--ai-panel)] hover:text-[var(--ai-text)] data-[state=open]:bg-[var(--ai-panel)] data-[state=open]:text-[var(--ai-text)]"
                    aria-label="Conversation options"
                  >
                    <MoreHorizontal className="size-4" />
                  </button>
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-52">
                  <DropdownMenuItem onSelect={openRenameDialog}>
                    <Pencil />
                    Rename conversation
                  </DropdownMenuItem>
                  <DropdownMenuItem
                    onSelect={() => void copyConversationLink()}
                  >
                    <Copy />
                    Copy conversation link
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem onSelect={resetPrototype}>
                    <RotateCcw />
                    Reset prototype
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </div>
          </div>

          <div className="min-h-0 flex-1 overflow-y-auto">
            <div className="mx-auto flex max-w-3xl flex-col gap-7 px-4 py-7 sm:px-6 sm:py-9">
              <AssistantMessage>
                <p className="text-base leading-7 text-[var(--ai-text)]">
                  Welcome to Temps. Describe the whole system you want to
                  ship—I’ll map its repositories, projects, and integrations,
                  then stop before anything changes.
                </p>
                <div className="mt-4 flex flex-wrap gap-2">
                  {[
                    'Deploy a multi-repo application',
                    'Connect private GitHub repos',
                    'Create shared infrastructure',
                  ].map((suggestion) => (
                    <button
                      key={suggestion}
                      type="button"
                      onClick={() => setComposer(suggestion)}
                      className="rounded-full border border-[var(--ai-line)] bg-[var(--ai-panel)] px-3 py-1.5 text-xs text-[var(--ai-muted)] transition-all hover:-translate-y-0.5 hover:border-[color-mix(in_srgb,var(--ai-lime)_38%,var(--ai-line))] hover:text-[var(--ai-text)]"
                    >
                      {suggestion}
                    </button>
                  ))}
                </div>
              </AssistantMessage>

              <UserMessage>
                Launch our commerce suite. It has three private GitHub repos: a
                Next.js storefront, a Go payments API, and a fulfillment worker.
                Add Postgres, Stripe, email, and pull-request previews.
              </UserMessage>

              <AssistantMessage label="Inspected 3 private repositories · 7.2s">
                <p className="leading-6">
                  I mapped the repositories into three Temps projects inside one
                  application stack. GitHub access stays in the connected GitHub
                  App; two integration values are missing, so I’ll collect them
                  outside this chat.
                </p>
                <DeploymentPlan
                  phase={phase}
                  applyStep={applyStep}
                  secretsReady={secretsReady}
                  missingCount={missingSecrets.length}
                  onAddSecrets={() => setSecretDialogOpen(true)}
                  onApply={applyPlan}
                />
                <SecretBoundary
                  references={secretReferences}
                  onAddSecrets={() => setSecretDialogOpen(true)}
                />
              </AssistantMessage>

              {exchanges.map((exchange) => (
                <div key={exchange.id} className="contents">
                  <UserMessage>{exchange.prompt}</UserMessage>
                  <AssistantMessage>
                    <p className="leading-6">{exchange.response}</p>
                  </AssistantMessage>
                </div>
              ))}

              {phase === 'live' && (
                <AssistantMessage label="Deployment verified">
                  <div className="rounded-xl border border-[rgba(215,255,99,0.28)] bg-[rgba(215,255,99,0.06)] p-4">
                    <div className="flex items-start gap-3">
                      <div className="mt-0.5 flex size-8 items-center justify-center rounded-full bg-[var(--ai-lime)] text-[var(--ai-lime-ink)]">
                        <Check className="size-4" />
                      </div>
                      <div>
                        <p className="font-medium text-[var(--ai-text)]">
                          Commerce suite is live
                        </p>
                        <p className="mt-1 text-sm leading-6 text-[var(--ai-muted)]">
                          All three projects are healthy, Stripe webhooks are
                          verified, Postgres is attached, and previews are
                          enabled for every repository.
                        </p>
                        <div className="mt-3 flex flex-wrap gap-2">
                          <button className="inline-flex items-center gap-1.5 rounded-lg bg-[var(--ai-lime)] px-3 py-1.5 text-xs font-semibold text-[var(--ai-lime-ink)]">
                            Open storefront
                            <ArrowRight className="size-3.5" />
                          </button>
                          <button className="rounded-lg border border-[var(--ai-line)] px-3 py-1.5 text-xs text-[var(--ai-muted)]">
                            Show deployment logs
                          </button>
                        </div>
                      </div>
                    </div>
                  </div>
                </AssistantMessage>
              )}
            </div>
          </div>

          <div className="border-t border-[var(--ai-line-soft)] bg-[color-mix(in_srgb,var(--ai-canvas)_88%,transparent)] px-3 pb-3 pt-2 backdrop-blur-xl sm:px-6 sm:pb-5">
            <div className="mx-auto max-w-3xl">
              {securityNotice && (
                <div className="mb-2 flex items-start gap-2 rounded-lg border border-[rgba(255,198,101,0.28)] bg-[rgba(255,198,101,0.06)] px-3 py-2 text-xs leading-5 text-[var(--ai-amber)]">
                  <AlertTriangle className="mt-0.5 size-3.5 shrink-0" />
                  <span>{securityNotice}</span>
                </div>
              )}
              <form
                onSubmit={submitPrompt}
                className="rounded-2xl border border-[var(--ai-line)] bg-[var(--ai-panel)] p-2 shadow-[0_20px_60px_rgba(0,0,0,0.32)] transition-colors focus-within:border-[color-mix(in_srgb,var(--ai-lime)_35%,var(--ai-line))]"
              >
                <Textarea
                  value={composer}
                  onChange={(event) => setComposer(event.target.value)}
                  onKeyDown={(event) => {
                    if (
                      event.key === 'Enter' &&
                      !event.shiftKey &&
                      !event.nativeEvent.isComposing
                    ) {
                      event.preventDefault()
                      event.currentTarget.form?.requestSubmit()
                    }
                  }}
                  placeholder="Tell Temps what outcome you want…"
                  className="min-h-11 resize-none border-0 bg-transparent px-2 py-2 text-sm text-[var(--ai-text)] shadow-none placeholder:text-[var(--ai-muted)] focus-visible:ring-0"
                  aria-label="Message Temps"
                />
                <div className="flex items-center justify-between gap-2 px-1">
                  <div className="flex items-center gap-1">
                    <button
                      type="button"
                      onClick={() => setSecretDialogOpen(true)}
                      className="flex size-7 items-center justify-center rounded-md text-[var(--ai-muted)] transition-colors hover:bg-[var(--ai-panel-raised)] hover:text-[var(--ai-text)]"
                      aria-label="Add secret securely"
                    >
                      <KeyRound className="size-3.5" />
                    </button>
                    <span className="hidden items-center gap-1 text-[10px] text-[var(--ai-muted)] sm:flex">
                      <EyeOff className="size-3" /> secrets blocked from chat
                    </span>
                  </div>
                  <button
                    type="submit"
                    disabled={!composer.trim()}
                    className="flex size-8 items-center justify-center rounded-lg bg-[var(--ai-lime)] text-[var(--ai-lime-ink)] transition-all hover:scale-[1.03] disabled:cursor-not-allowed disabled:opacity-30"
                    aria-label="Send message"
                  >
                    <ArrowUp className="size-4" />
                  </button>
                </div>
              </form>
              <p className="mt-2 text-center text-[10px] text-[var(--ai-muted)]">
                Temps can make mistakes. Every write is shown for approval and
                recorded in the audit log.
              </p>
            </div>
          </div>
        </main>

        <GeneratedCanvas
          phase={phase}
          applyStep={applyStep}
          secretReferences={secretReferences}
        />
      </div>

      <SecureSecretDialog
        open={secretDialogOpen}
        onOpenChange={setSecretDialogOpen}
        requirements={
          missingSecrets.length > 0 ? missingSecrets : SECRET_REQUIREMENTS
        }
        onStore={storeSecrets}
      />
      <Dialog open={renameDialogOpen} onOpenChange={setRenameDialogOpen}>
        <DialogContent className="sm:max-w-md">
          <form onSubmit={renameConversation}>
            <DialogHeader>
              <DialogTitle>Rename conversation</DialogTitle>
              <DialogDescription>
                Use a name that describes the application or operation this
                thread manages.
              </DialogDescription>
            </DialogHeader>
            <div className="py-5">
              <Label htmlFor="conversation-title">Conversation name</Label>
              <Input
                id="conversation-title"
                value={renameDraft}
                onChange={(event) => setRenameDraft(event.target.value)}
                autoFocus
                className="mt-2"
              />
            </div>
            <DialogFooter>
              <Button
                type="button"
                variant="outline"
                onClick={() => setRenameDialogOpen(false)}
              >
                Cancel
              </Button>
              <Button type="submit" disabled={!renameDraft.trim()}>
                Save name
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </div>
  )
}

export function AiFirstPrototype() {
  return <AiFirstWorkspace />
}

function ConversationRail({
  open,
  activeTitle,
  onClose,
}: {
  open: boolean
  activeTitle: string
  onClose: () => void
}) {
  return (
    <aside
      className={cn(
        'absolute inset-y-0 left-0 z-30 flex w-[230px] flex-col border-r border-[var(--ai-line)] bg-[var(--ai-panel)] transition-transform duration-300 lg:relative lg:translate-x-0',
        open ? 'translate-x-0' : '-translate-x-full'
      )}
    >
      <div className="flex items-center justify-between px-3 py-3">
        <span className="text-[10px] font-semibold uppercase tracking-[0.18em] text-[var(--ai-muted)]">
          Threads
        </span>
        <div className="flex gap-1">
          <button
            type="button"
            className="flex size-7 items-center justify-center rounded-md border border-[var(--ai-line)] text-[var(--ai-muted)] hover:text-[var(--ai-text)]"
            aria-label="New thread"
          >
            <Plus className="size-3.5" />
          </button>
          <button
            type="button"
            className="flex size-7 items-center justify-center rounded-md text-[var(--ai-muted)] lg:hidden"
            onClick={onClose}
            aria-label="Close conversations"
          >
            <X className="size-3.5" />
          </button>
        </div>
      </div>

      <div className="space-y-1 px-2">
        {conversations.map((conversation) => (
          <button
            key={conversation.title}
            type="button"
            className={cn(
              'w-full rounded-lg px-3 py-2.5 text-left transition-colors',
              conversation.active
                ? 'bg-[var(--ai-panel-raised)] text-[var(--ai-text)]'
                : 'text-[var(--ai-muted)] hover:bg-[var(--ai-panel-raised)] hover:text-[var(--ai-text)]'
            )}
          >
            <span className="block truncate text-xs font-medium">
              {conversation.active ? activeTitle : conversation.title}
            </span>
            <span className="mt-1 block text-[10px] text-[var(--ai-muted)]">
              {conversation.detail}
            </span>
          </button>
        ))}
      </div>

      <div className="mx-3 mt-5 border-t border-[var(--ai-line-soft)] pt-4">
        <p className="px-2 text-[10px] font-semibold uppercase tracking-[0.16em] text-[var(--ai-muted)]">
          Context
        </p>
        <div className="mt-2 space-y-1">
          <ContextLink icon={Server} label="local instance" detail="healthy" />
          <ContextLink
            icon={GitBranch}
            label="3 private repos"
            detail="GitHub"
          />
          <ContextLink
            icon={Network}
            label="commerce suite"
            detail="3 projects"
          />
          <ContextLink icon={ShieldCheck} label="Guarded mode" detail="on" />
        </div>
      </div>

      <div className="mt-auto border-t border-[var(--ai-line)] p-3">
        <button
          type="button"
          className="flex w-full items-center gap-2.5 rounded-lg px-2 py-2 text-left text-xs text-[var(--ai-muted)] transition-colors hover:bg-[var(--ai-panel-raised)] hover:text-[var(--ai-text)]"
        >
          <div className="flex size-7 items-center justify-center rounded-full bg-[#242a24] text-[10px] font-semibold text-[var(--ai-text)]">
            OP
          </div>
          <span className="min-w-0 flex-1 truncate">Platform admin</span>
          <Settings2 className="size-3.5" />
        </button>
      </div>
    </aside>
  )
}

function ContextLink({
  icon: Icon,
  label,
  detail,
}: {
  icon: typeof Server
  label: string
  detail: string
}) {
  return (
    <button
      type="button"
      className="flex w-full items-center gap-2 rounded-md px-2 py-2 text-xs text-[var(--ai-muted)] hover:bg-[var(--ai-panel-raised)] hover:text-[var(--ai-text)]"
    >
      <Icon className="size-3.5" />
      <span className="min-w-0 flex-1 truncate text-left">{label}</span>
      <span className="text-[9px] text-[var(--ai-muted)]">{detail}</span>
    </button>
  )
}

function AssistantMessage({
  children,
  label,
}: {
  children: React.ReactNode
  label?: string
}) {
  return (
    <div className="grid grid-cols-[28px_minmax(0,1fr)] gap-3 animate-in fade-in-0 slide-in-from-bottom-2 duration-500">
      <div className="flex size-7 items-center justify-center rounded-lg border border-[rgba(215,255,99,0.22)] bg-[rgba(215,255,99,0.08)] text-[var(--ai-lime)]">
        <Sparkles className="size-3.5" />
      </div>
      <div className="min-w-0 text-sm text-[var(--ai-muted)]">
        <div className="mb-2 flex items-center gap-2">
          <span className="text-xs font-medium text-[var(--ai-text)]">
            Temps
          </span>
          {label && <span className="text-[10px]">{label}</span>}
        </div>
        {children}
      </div>
    </div>
  )
}

function UserMessage({ children }: { children: React.ReactNode }) {
  return (
    <div className="ml-8 flex justify-end animate-in fade-in-0 slide-in-from-bottom-2 duration-500">
      <div className="max-w-[88%] rounded-2xl rounded-tr-md border border-[var(--ai-line)] bg-[var(--ai-panel-raised)] px-4 py-3 text-sm leading-6 text-[var(--ai-text)] shadow-sm">
        {children}
      </div>
    </div>
  )
}

function DeploymentPlan({
  phase,
  applyStep,
  secretsReady,
  missingCount,
  onAddSecrets,
  onApply,
}: {
  phase: DeploymentPhase
  applyStep: number
  secretsReady: boolean
  missingCount: number
  onAddSecrets: () => void
  onApply: () => void
}) {
  return (
    <div className="mt-4 overflow-hidden rounded-xl border border-[var(--ai-line)] bg-[var(--ai-panel)] shadow-[0_18px_50px_rgba(0,0,0,0.18)]">
      <div className="flex items-center justify-between border-b border-[var(--ai-line-soft)] px-4 py-3">
        <div className="flex items-center gap-2">
          <Wand2 className="size-3.5 text-[var(--ai-lime)]" />
          <span className="text-xs font-medium text-[var(--ai-text)]">
            Generated application plan
          </span>
        </div>
        <span className="text-[10px] text-[var(--ai-muted)]">
          94% confidence
        </span>
      </div>

      <div className="grid gap-px bg-[var(--ai-line-soft)] sm:grid-cols-2">
        <PlanDatum icon={GitBranch} label="Source" value="3 private repos" />
        <PlanDatum
          icon={Code2}
          label="Detected"
          value="Next.js · Go · worker"
        />
        <PlanDatum icon={Database} label="Data" value="Shared Postgres 17" />
        <PlanDatum
          icon={Network}
          label="Delivery"
          value="Production + previews"
        />
      </div>

      <div className="border-t border-[var(--ai-line-soft)] px-4 py-3">
        <div className="mb-2 flex items-center justify-between">
          <p className="text-[9px] font-medium uppercase tracking-[0.14em] text-[var(--ai-muted)]">
            Projects in this application
          </p>
          <span className="inline-flex items-center gap-1 text-[9px] text-[var(--ai-muted)]">
            <Lock className="size-2.5" /> private via GitHub App
          </span>
        </div>
        <div className="grid gap-1.5 sm:grid-cols-3">
          <StackProject
            name="storefront-web"
            runtime="Next.js"
            dependsOn="payments-api"
          />
          <StackProject
            name="payments-api"
            runtime="Go"
            dependsOn="Postgres + Stripe"
          />
          <StackProject
            name="fulfillment-worker"
            runtime="Node worker"
            dependsOn="Postgres + email"
          />
        </div>
      </div>

      <div className="border-t border-[var(--ai-line-soft)] px-4 py-3">
        <div className="flex items-center justify-between gap-4">
          <div>
            <p className="text-xs font-medium text-[var(--ai-text)]">
              11 proposed changes across 3 projects
            </p>
            <p className="mt-0.5 text-[10px] text-[var(--ai-muted)]">
              Create projects, shared services, integrations, and preview
              policies
            </p>
          </div>
          {phase === 'live' ? (
            <span className="inline-flex items-center gap-1.5 rounded-lg bg-[rgba(215,255,99,0.1)] px-3 py-2 text-xs font-medium text-[var(--ai-lime)]">
              <CheckCircle2 className="size-3.5" /> Applied
            </span>
          ) : phase === 'applying' ? (
            <span className="inline-flex items-center gap-2 rounded-lg border border-[var(--ai-line)] px-3 py-2 text-xs text-[var(--ai-muted)]">
              <LoaderCircle className="size-3.5 animate-spin motion-reduce:animate-none" />
              Applying {Math.min(applyStep + 1, 3)}/3
            </span>
          ) : !secretsReady ? (
            <button
              type="button"
              onClick={onAddSecrets}
              className="shrink-0 rounded-lg bg-[var(--ai-lime)] px-3 py-2 text-xs font-semibold text-[var(--ai-lime-ink)] transition-transform hover:scale-[1.02]"
            >
              Add {missingCount} secrets securely
            </button>
          ) : (
            <AlertDialog>
              <AlertDialogTrigger asChild>
                <button
                  type="button"
                  className="shrink-0 rounded-lg bg-[var(--ai-lime)] px-3 py-2 text-xs font-semibold text-[var(--ai-lime-ink)] transition-transform hover:scale-[1.02]"
                >
                  Review & apply
                </button>
              </AlertDialogTrigger>
              <AlertDialogContent>
                <AlertDialogHeader>
                  <AlertDialogTitle>Apply 11 changes?</AlertDialogTitle>
                  <AlertDialogDescription>
                    Temps will create three linked projects, shared Postgres,
                    production and preview environments, a domain, and scoped
                    Stripe and email bindings. It will not delete or modify
                    existing resources.
                  </AlertDialogDescription>
                </AlertDialogHeader>
                <div className="rounded-lg border bg-muted/40 p-3 text-sm">
                  <div className="flex items-center gap-2 font-medium">
                    <ShieldCheck className="size-4 text-emerald-600" />
                    One-time scoped approval
                  </div>
                  <p className="mt-1 text-xs leading-5 text-muted-foreground">
                    This grants only the eleven writes shown above, across the
                    three named projects. Future changes need a new approval.
                  </p>
                </div>
                <AlertDialogFooter>
                  <AlertDialogCancel>Keep reviewing</AlertDialogCancel>
                  <AlertDialogAction onClick={onApply}>
                    Apply 11 changes
                  </AlertDialogAction>
                </AlertDialogFooter>
              </AlertDialogContent>
            </AlertDialog>
          )}
        </div>
      </div>
    </div>
  )
}

function PlanDatum({
  icon: Icon,
  label,
  value,
}: {
  icon: typeof GitBranch
  label: string
  value: string
}) {
  return (
    <div className="flex items-center gap-3 bg-[var(--ai-panel)] px-4 py-3">
      <Icon className="size-3.5 text-[var(--ai-muted)]" />
      <div className="min-w-0">
        <p className="text-[9px] font-medium uppercase tracking-[0.14em] text-[var(--ai-muted)]">
          {label}
        </p>
        <p className="mt-0.5 truncate text-xs text-[var(--ai-text)]">{value}</p>
      </div>
    </div>
  )
}

function StackProject({
  name,
  runtime,
  dependsOn,
}: {
  name: string
  runtime: string
  dependsOn: string
}) {
  return (
    <div className="rounded-lg border border-[var(--ai-line)] bg-[var(--ai-canvas)] p-2.5">
      <div className="flex items-center gap-1.5">
        <Lock className="size-2.5 text-[var(--ai-muted)]" />
        <p className="truncate font-mono text-[9px] text-[var(--ai-text)]">
          {name}
        </p>
      </div>
      <p className="mt-1.5 text-[10px] text-[var(--ai-muted)]">{runtime}</p>
      <p className="mt-0.5 truncate text-[9px] text-[var(--ai-lime)]">
        → {dependsOn}
      </p>
    </div>
  )
}

function SecretBoundary({
  references,
  onAddSecrets,
}: {
  references: SecretReference[]
  onAddSecrets: () => void
}) {
  return (
    <div className="mt-3 rounded-xl border border-[rgba(255,198,101,0.2)] bg-[rgba(255,198,101,0.04)] p-3.5">
      <div className="flex items-start gap-3">
        <div className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-[rgba(255,198,101,0.1)] text-[var(--ai-amber)]">
          <LockKeyhole className="size-4" />
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-xs font-medium text-[var(--ai-text)]">
              Secret boundary
            </p>
            <button
              type="button"
              onClick={onAddSecrets}
              className="text-[10px] font-medium text-[var(--ai-amber)] hover:underline"
            >
              {references.length > 0 ? 'Manage references' : 'Add securely'}
            </button>
          </div>
          <p className="mt-1 text-xs leading-5 text-[var(--ai-muted)]">
            Values travel directly to the encrypted vault. The model sees only
            names, scopes, and opaque references.
          </p>
          <div className="mt-2 flex flex-wrap gap-1.5">
            {SECRET_REQUIREMENTS.map((requirement) => {
              const stored = references.some(
                (reference) => reference.key === requirement.key
              )
              return (
                <span
                  key={requirement.key}
                  className={cn(
                    'inline-flex items-center gap-1 rounded-md border px-2 py-1 font-mono text-[9px]',
                    stored
                      ? 'border-[rgba(215,255,99,0.2)] bg-[rgba(215,255,99,0.06)] text-[var(--ai-lime)]'
                      : 'border-[var(--ai-line)] text-[var(--ai-muted)]'
                  )}
                >
                  {stored ? (
                    <Check className="size-2.5" />
                  ) : (
                    <EyeOff className="size-2.5" />
                  )}
                  {requirement.key}
                  <span className="font-sans text-[8px] opacity-70">
                    → {requirement.targets.length} project
                    {requirement.targets.length === 1 ? '' : 's'}
                  </span>
                </span>
              )
            })}
          </div>
        </div>
      </div>
    </div>
  )
}

function GeneratedCanvas({
  phase,
  applyStep,
  secretReferences,
}: {
  phase: DeploymentPhase
  applyStep: number
  secretReferences: SecretReference[]
}) {
  return (
    <aside className="hidden min-h-0 overflow-y-auto border-l border-[var(--ai-line)] bg-[var(--ai-panel)] xl:block">
      <div className="border-b border-[var(--ai-line)] px-4 py-4">
        <div className="flex items-center justify-between">
          <div>
            <p className="text-[10px] font-semibold uppercase tracking-[0.16em] text-[var(--ai-muted)]">
              Generated view
            </p>
            <h2 className="mt-1 text-sm font-medium">
              Commerce suite topology
            </h2>
          </div>
          <Zap className="size-4 text-[var(--ai-lime)]" />
        </div>
      </div>

      <div className="space-y-5 p-4">
        <section className="rounded-xl border border-[var(--ai-line)] bg-[var(--ai-canvas)] p-3">
          <div className="flex items-center justify-between gap-2 rounded-lg border border-[var(--ai-line-soft)] bg-[var(--ai-panel)] px-2.5 py-2">
            <div className="flex items-center gap-2 text-[11px] font-medium">
              <GitBranch className="size-3.5 text-[var(--ai-lime)]" />
              GitHub App
            </div>
            <span className="inline-flex items-center gap-1 text-[9px] text-[var(--ai-muted)]">
              <Lock className="size-2.5" /> 3 private repos
            </span>
          </div>
          <div className="ml-[14px] h-4 border-l border-dashed border-[var(--ai-line)]" />
          <div className="space-y-1.5">
            <TopologyProject
              name="storefront-web"
              runtime="Next.js · public edge"
              dependency="calls payments-api"
            />
            <TopologyProject
              name="payments-api"
              runtime="Go · private service"
              dependency="Postgres + Stripe"
            />
            <TopologyProject
              name="fulfillment-worker"
              runtime="Node · background worker"
              dependency="Postgres + email"
            />
          </div>
          <div className="ml-[14px] h-4 border-l border-dashed border-[var(--ai-line)]" />
          <div className="grid grid-cols-2 gap-2">
            <TopologyNode
              icon={Database}
              label="Postgres"
              detail="shared data"
            />
            <TopologyNode
              icon={CreditCard}
              label="Stripe"
              detail="payments-api"
            />
            <div className="col-span-2">
              <TopologyNode
                icon={Globe2}
                label="store.example.com"
                detail="storefront-web · automatic TLS"
              />
            </div>
          </div>
        </section>

        <section>
          <div className="mb-2 flex items-center justify-between">
            <h3 className="text-[10px] font-semibold uppercase tracking-[0.16em] text-[var(--ai-muted)]">
              Execution
            </h3>
            <span
              className={cn(
                'text-[10px]',
                phase === 'live'
                  ? 'text-[var(--ai-lime)]'
                  : 'text-[var(--ai-muted)]'
              )}
            >
              {phase === 'review'
                ? 'waiting for approval'
                : phase === 'applying'
                  ? 'in progress'
                  : 'verified'}
            </span>
          </div>
          <div className="space-y-1.5">
            <ExecutionRow
              label="Create 3 linked projects"
              done={phase === 'live' || applyStep >= 1}
              active={phase === 'applying' && applyStep === 0}
            />
            <ExecutionRow
              label="Bind shared services"
              done={phase === 'live' || applyStep >= 2}
              active={phase === 'applying' && applyStep === 1}
            />
            <ExecutionRow
              label="Deploy all and verify"
              done={phase === 'live'}
              active={phase === 'applying' && applyStep === 2}
            />
          </div>
        </section>

        <section className="rounded-xl border border-[rgba(215,255,99,0.18)] bg-[rgba(215,255,99,0.035)] p-3">
          <div className="flex items-center gap-2">
            <ShieldCheck className="size-4 text-[var(--ai-lime)]" />
            <h3 className="text-xs font-medium">AI access policy</h3>
          </div>
          <div className="mt-3 space-y-2.5">
            <PolicyRow
              label="Read private repo metadata"
              value="Via GitHub App"
            />
            <PolicyRow label="Create resources" value="Ask each time" />
            <PolicyRow label="Read GitHub access token" value="Never" danger />
            <PolicyRow label="Read secret values" value="Never" danger />
            <PolicyRow label="Delete resources" value="Denied" danger />
          </div>
        </section>

        <section>
          <div className="mb-2 flex items-center justify-between">
            <h3 className="text-[10px] font-semibold uppercase tracking-[0.16em] text-[var(--ai-muted)]">
              Model-visible context
            </h3>
            <EyeOff className="size-3.5 text-[var(--ai-muted)]" />
          </div>
          <div className="space-y-1.5 rounded-xl border border-[var(--ai-line)] bg-[var(--ai-canvas)] p-3 font-mono text-[9px] leading-5 text-[var(--ai-muted)]">
            <p>stack.name: commerce-suite</p>
            <p>projects: [web, payments-api, worker]</p>
            <p>repositories.auth: brokered_github_app</p>
            <p>approval.mode: per_action</p>
            {secretReferences.length > 0 ? (
              secretReferences.map((secret) => (
                <p key={secret.key} className="truncate text-[var(--ai-lime)]">
                  {secret.key} → [{secret.targets.join(',')}]:{' '}
                  {secret.reference}
                </p>
              ))
            ) : (
              <p>secrets: [names_only, values_hidden]</p>
            )}
          </div>
        </section>
      </div>
    </aside>
  )
}

function TopologyProject({
  name,
  runtime,
  dependency,
}: {
  name: string
  runtime: string
  dependency: string
}) {
  return (
    <div className="flex items-center gap-2.5 rounded-lg border border-[var(--ai-line)] bg-[var(--ai-panel)] p-2.5">
      <div className="flex size-7 shrink-0 items-center justify-center rounded-md bg-[rgba(215,255,99,0.08)] text-[var(--ai-lime)]">
        <Code2 className="size-3.5" />
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-1.5">
          <p className="truncate font-mono text-[10px] font-medium">{name}</p>
          <Lock className="size-2.5 shrink-0 text-[var(--ai-muted)]" />
        </div>
        <p className="mt-0.5 truncate text-[9px] text-[var(--ai-muted)]">
          {runtime}
        </p>
      </div>
      <span className="max-w-20 text-right text-[8px] leading-3 text-[var(--ai-lime)]">
        {dependency}
      </span>
    </div>
  )
}

function TopologyNode({
  icon: Icon,
  label,
  detail,
}: {
  icon: typeof Code2
  label: string
  detail: string
}) {
  return (
    <div className="rounded-lg border border-[var(--ai-line)] bg-[var(--ai-panel)] p-2.5">
      <Icon className="size-3.5 text-[var(--ai-lime)]" />
      <p className="mt-2 truncate text-[11px] font-medium">{label}</p>
      <p className="mt-0.5 truncate text-[9px] text-[var(--ai-muted)]">
        {detail}
      </p>
    </div>
  )
}

function ExecutionRow({
  label,
  done,
  active,
}: {
  label: string
  done: boolean
  active: boolean
}) {
  return (
    <div className="flex items-center gap-2 rounded-lg border border-[var(--ai-line-soft)] bg-[var(--ai-canvas)] px-2.5 py-2 text-[11px]">
      {done ? (
        <CheckCircle2 className="size-3.5 text-[var(--ai-lime)]" />
      ) : active ? (
        <LoaderCircle className="size-3.5 animate-spin text-[var(--ai-lime)] motion-reduce:animate-none" />
      ) : (
        <Circle className="size-3.5 text-[var(--ai-line)]" />
      )}
      <span
        className={done ? 'text-[var(--ai-text)]' : 'text-[var(--ai-muted)]'}
      >
        {label}
      </span>
    </div>
  )
}

function PolicyRow({
  label,
  value,
  danger = false,
}: {
  label: string
  value: string
  danger?: boolean
}) {
  return (
    <div className="flex items-center justify-between gap-3 text-[10px]">
      <span className="text-[var(--ai-muted)]">{label}</span>
      <span
        className={danger ? 'text-[var(--ai-amber)]' : 'text-[var(--ai-lime)]'}
      >
        {value}
      </span>
    </div>
  )
}

function SecureSecretDialog({
  open,
  onOpenChange,
  requirements,
  onStore,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  requirements: ReadonlyArray<{
    key: string
    description: string
    targets: readonly string[]
  }>
  onStore: (drafts: SecretDraft[]) => void
}) {
  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    const form = event.currentTarget
    const data = new FormData(form)
    const drafts = requirements.map((requirement) => ({
      key: requirement.key,
      value: String(data.get(requirement.key) ?? ''),
      scope: 'production' as const,
      targets: [...requirement.targets],
    }))

    onStore(drafts)
    form.reset()
    onOpenChange(false)
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <div className="mb-2 flex size-10 items-center justify-center rounded-xl bg-emerald-500/10 text-emerald-600 dark:text-emerald-400">
            <LockKeyhole className="size-5" />
          </div>
          <DialogTitle>Secure secret broker</DialogTitle>
          <DialogDescription>
            Values are encrypted directly into the application vault and bound
            only to the selected projects. They are not added to the
            conversation, model context, browser history, or audit log payloads.
          </DialogDescription>
        </DialogHeader>

        <form onSubmit={submit} autoComplete="off" className="space-y-4">
          <div className="rounded-lg border border-emerald-500/20 bg-emerald-500/5 p-3">
            <div className="flex items-start gap-2.5">
              <ShieldCheck className="mt-0.5 size-4 shrink-0 text-emerald-600 dark:text-emerald-400" />
              <div>
                <p className="text-xs font-medium">AI-isolated input</p>
                <p className="mt-1 text-xs leading-5 text-muted-foreground">
                  The assistant will receive only an opaque reference such as{' '}
                  <code className="rounded bg-muted px-1 py-0.5 text-[10px]">
                    secret://…/STRIPE_SECRET_KEY
                  </code>
                  .
                </p>
              </div>
            </div>
          </div>

          {requirements.map((requirement) => (
            <div key={requirement.key} className="space-y-1.5">
              <div className="flex items-center justify-between gap-3">
                <Label
                  htmlFor={`secure-${requirement.key}`}
                  className="font-mono text-xs"
                >
                  {requirement.key}
                </Label>
                <span className="text-[10px] text-muted-foreground">
                  {requirement.description}
                </span>
              </div>
              <Input
                id={`secure-${requirement.key}`}
                name={requirement.key}
                type="password"
                required
                minLength={8}
                placeholder="Enter value securely"
                autoComplete="new-password"
                className="font-mono"
              />
              <p className="flex items-center gap-1.5 text-[10px] text-muted-foreground">
                <Network className="size-3" />
                Bound to {requirement.targets.join(', ')}
              </p>
            </div>
          ))}

          <div className="flex items-center justify-between rounded-lg border px-3 py-2 text-xs">
            <div className="flex items-center gap-2">
              <Cloud className="size-3.5 text-muted-foreground" />
              <span>Scope</span>
            </div>
            <span className="font-medium">Production only</span>
          </div>

          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit">
              <LockKeyhole className="mr-1.5 size-3.5" />
              Encrypt & store references
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
