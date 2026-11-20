// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { QuestionKind, SurveyResponse } from '@/api/client'
import {
  createSurveyMutation,
  updateSurveyMutation,
} from '@/api/client/@tanstack/react-query.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useMutation } from '@tanstack/react-query'
import { ArrowDown, ArrowUp, Loader2, Plus, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import {
  blankQuestion,
  draftFromSurvey,
  draftToBody,
  emptyDraft,
  hasChoices,
  hasErrors,
  isScale,
  QUESTION_KINDS,
  RATING_SCALES,
  surveyErrorMessage,
  validateDraft,
  type DraftErrors,
  type QuestionDraft,
  type SurveyDraft,
} from './survey-form'

interface SurveyEditorDialogProps {
  projectId: number
  /** The survey to edit; omit to create one. */
  survey?: SurveyResponse
  open: boolean
  onOpenChange: (open: boolean) => void
  onSaved: (survey: SurveyResponse) => void
}

export function SurveyEditorDialog({
  projectId,
  survey,
  open,
  onOpenChange,
  onSaved,
}: SurveyEditorDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-2xl">
        {/* The content unmounts when the dialog closes, so each opening
            starts from the stored survey (or a blank draft). */}
        <SurveyEditorForm
          projectId={projectId}
          survey={survey}
          onOpenChange={onOpenChange}
          onSaved={onSaved}
        />
      </DialogContent>
    </Dialog>
  )
}

function SurveyEditorForm({
  projectId,
  survey,
  onOpenChange,
  onSaved,
}: Omit<SurveyEditorDialogProps, 'open'>) {
  const [draft, setDraft] = useState<SurveyDraft>(() =>
    survey ? draftFromSurvey(survey) : emptyDraft()
  )
  // The revision the draft was made from. A refetch while the dialog is open
  // updates `survey`, but the draft still holds the older state: saving it
  // under the newer revision would silently replace someone else's edit,
  // while the original one makes the server refuse it with 409.
  const [baseRevision] = useState(() => survey?.updated_at)
  const [errors, setErrors] = useState<DraftErrors>({ question: {} })

  const onError = (error: unknown) =>
    toast.error(surveyErrorMessage(error, 'Could not save the survey'))
  const create = useMutation({
    ...createSurveyMutation(),
    onSuccess: (saved) => {
      toast.success(`Survey "${saved.name}" created as a draft`)
      onOpenChange(false)
      onSaved(saved)
    },
    onError,
  })
  const update = useMutation({
    ...updateSurveyMutation(),
    onSuccess: (saved) => {
      toast.success(`Survey "${saved.name}" saved`)
      onOpenChange(false)
      onSaved(saved)
    },
    onError,
  })
  const pending = create.isPending || update.isPending

  const patch = (next: Partial<SurveyDraft>) =>
    setDraft((d) => ({ ...d, ...next }))
  const patchQuestion = (index: number, next: Partial<QuestionDraft>) =>
    setDraft((d) => ({
      ...d,
      questions: d.questions.map((q, i) =>
        i === index ? { ...q, ...next } : q
      ),
    }))
  const moveQuestion = (index: number, by: number) =>
    setDraft((d) => {
      const questions = [...d.questions]
      const [moved] = questions.splice(index, 1)
      questions.splice(index + by, 0, moved)
      return { ...d, questions }
    })

  const handleSubmit = (event: React.FormEvent) => {
    event.preventDefault()
    const found = validateDraft(draft, survey?.status === 'active')
    setErrors(found)
    if (hasErrors(found)) return
    const body = draftToBody(draft)
    if (survey) {
      update.mutate({
        path: { project_id: projectId, survey_id: survey.id },
        body: { ...body, expected_updated_at: baseRevision },
      })
    } else {
      create.mutate({
        path: { project_id: projectId },
        body: { ...body, status: 'draft' },
      })
    }
  }

  return (
    <form onSubmit={handleSubmit} className="space-y-6" noValidate>
      <DialogHeader>
        <DialogTitle>
          {survey ? `Edit ${survey.name}` : 'New survey'}
        </DialogTitle>
        <DialogDescription>
          Ask visitors a question inside your app. Answers arrive as analytics
          events and are summarised on the survey&apos;s page.
        </DialogDescription>
      </DialogHeader>

      <div className="space-y-2">
        <Label htmlFor="survey-name">Name</Label>
        <Input
          id="survey-name"
          value={draft.name}
          onChange={(e) => patch({ name: e.target.value })}
          placeholder="Checkout feedback"
          aria-invalid={Boolean(errors.name)}
        />
        <p className="text-xs text-muted-foreground">
          Shown as the widget&apos;s title.
        </p>
        {errors.name && (
          <p className="text-sm text-destructive">{errors.name}</p>
        )}
      </div>
      <div className="space-y-2">
        <Label htmlFor="survey-description">Description (internal)</Label>
        <Textarea
          id="survey-description"
          value={draft.description}
          onChange={(e) => patch({ description: e.target.value })}
          rows={2}
        />
      </div>

      <section className="space-y-3" aria-labelledby="survey-questions-heading">
        <div className="flex items-center justify-between">
          <h3 id="survey-questions-heading" className="text-sm font-medium">
            Questions
          </h3>
          <Button
            type="button"
            variant="outline"
            size="sm"
            disabled={draft.questions.length >= 20}
            onClick={() =>
              patch({
                questions: [
                  ...draft.questions,
                  blankQuestion(draft.questions, 'open_text'),
                ],
              })
            }
          >
            <Plus className="mr-1.5 h-4 w-4" />
            Add question
          </Button>
        </div>
        {errors.questions && (
          <p className="text-sm text-destructive">{errors.questions}</p>
        )}
        {draft.questions.map((question, index) => (
          <QuestionEditor
            key={index}
            index={index}
            count={draft.questions.length}
            question={question}
            error={errors.question[index]}
            onChange={(next) => patchQuestion(index, next)}
            onMove={(by) => moveQuestion(index, by)}
            onRemove={() =>
              patch({
                questions: draft.questions.filter((_, i) => i !== index),
              })
            }
          />
        ))}
      </section>

      <section className="space-y-4" aria-labelledby="survey-display-heading">
        <h3 id="survey-display-heading" className="text-sm font-medium">
          When to show it
        </h3>
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <div className="space-y-2">
            <Label htmlFor="survey-path">Page path</Label>
            <Input
              id="survey-path"
              value={draft.pathPattern}
              onChange={(e) => patch({ pathPattern: e.target.value })}
              placeholder="/checkout/*  (empty: every page)"
            />
            {errors.pathPattern && (
              <p className="text-sm text-destructive">{errors.pathPattern}</p>
            )}
          </div>
          <div className="space-y-2">
            <Label htmlFor="survey-trigger">After this event</Label>
            <Input
              id="survey-trigger"
              value={draft.triggerEvent}
              onChange={(e) => patch({ triggerEvent: e.target.value })}
              placeholder="checkout_completed  (empty: on page load)"
            />
            {errors.triggerEvent && (
              <p className="text-sm text-destructive">{errors.triggerEvent}</p>
            )}
          </div>
          <div className="space-y-2">
            <Label htmlFor="survey-sample">Share of visitors (%)</Label>
            <Input
              id="survey-sample"
              inputMode="decimal"
              value={draft.samplePercent}
              onChange={(e) => patch({ samplePercent: e.target.value })}
            />
            <p className="text-xs text-muted-foreground">
              Below 100%, only visitors Temps can recognise (the visitor cookie
              or a key you pass) are sampled, so the choice holds across pages.
            </p>
            {errors.samplePercent && (
              <p className="text-sm text-destructive">{errors.samplePercent}</p>
            )}
          </div>
          <div className="flex items-start justify-between gap-4 rounded-md border p-3">
            <div>
              <Label htmlFor="survey-once">Once per visitor</Label>
              <p className="text-xs text-muted-foreground">
                Never show it again to someone who saw it.
              </p>
            </div>
            <Switch
              id="survey-once"
              checked={draft.oncePerVisitor}
              onCheckedChange={(checked) => patch({ oncePerVisitor: checked })}
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="survey-flag">Only where this flag…</Label>
            <Input
              id="survey-flag"
              value={draft.flagKey}
              onChange={(e) => patch({ flagKey: e.target.value })}
              placeholder="checkout.v2  (empty: no flag)"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="survey-flag-value">…serves this value</Label>
            <Input
              id="survey-flag-value"
              value={draft.flagValue}
              disabled={!draft.flagKey.trim()}
              onChange={(e) => patch({ flagValue: e.target.value })}
            />
            {errors.flagValue && (
              <p className="text-sm text-destructive">{errors.flagValue}</p>
            )}
          </div>
        </div>
      </section>

      <DialogFooter>
        <Button
          type="button"
          variant="outline"
          onClick={() => onOpenChange(false)}
        >
          Cancel
        </Button>
        <Button type="submit" disabled={pending}>
          {pending && <Loader2 className="mr-1.5 h-4 w-4 animate-spin" />}
          {survey ? 'Save' : 'Create draft'}
        </Button>
      </DialogFooter>
    </form>
  )
}

interface QuestionEditorProps {
  index: number
  count: number
  question: QuestionDraft
  error?: string
  onChange: (next: Partial<QuestionDraft>) => void
  onMove: (by: number) => void
  onRemove: () => void
}

function QuestionEditor({
  index,
  count,
  question,
  error,
  onChange,
  onMove,
  onRemove,
}: QuestionEditorProps) {
  const base = `survey-q${index}`
  const changeKind = (kind: QuestionKind) =>
    onChange({
      kind,
      choices: hasChoices(kind)
        ? question.choices.length >= 2
          ? question.choices
          : ['', '']
        : [],
    })
  return (
    <fieldset className="space-y-3 rounded-md border p-3">
      <legend className="sr-only">Question {index + 1}</legend>
      <div className="flex flex-col gap-2 sm:flex-row sm:items-end">
        <div className="flex-1 space-y-1">
          <Label htmlFor={`${base}-prompt`}>Question {index + 1}</Label>
          <Input
            id={`${base}-prompt`}
            value={question.prompt}
            onChange={(e) => onChange({ prompt: e.target.value })}
          />
        </div>
        <Select
          value={question.kind}
          onValueChange={(v) => changeKind(v as QuestionKind)}
        >
          <SelectTrigger
            className="w-full sm:w-[170px]"
            aria-label="Question type"
          >
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {QUESTION_KINDS.map((kind) => (
              <SelectItem key={kind.value} value={kind.value}>
                {kind.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <div className="flex gap-1">
          <Button
            type="button"
            variant="ghost"
            size="icon"
            disabled={index === 0}
            onClick={() => onMove(-1)}
            aria-label="Move up"
          >
            <ArrowUp className="h-4 w-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            disabled={index === count - 1}
            onClick={() => onMove(1)}
            aria-label="Move down"
          >
            <ArrowDown className="h-4 w-4" />
          </Button>
          <Button
            type="button"
            variant="ghost"
            size="icon"
            onClick={onRemove}
            aria-label={`Remove question ${index + 1}`}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        </div>
      </div>

      {hasChoices(question.kind) && (
        <div className="space-y-2">
          {question.choices.map((choice, i) => (
            <div key={i} className="flex gap-2">
              <Input
                aria-label={`Choice ${i + 1}`}
                value={choice}
                onChange={(e) =>
                  onChange({
                    choices: question.choices.map((c, j) =>
                      j === i ? e.target.value : c
                    ),
                  })
                }
              />
              <Button
                type="button"
                variant="ghost"
                size="icon"
                disabled={question.choices.length <= 2}
                aria-label={`Remove choice ${i + 1}`}
                onClick={() =>
                  onChange({
                    choices: question.choices.filter((_, j) => j !== i),
                  })
                }
              >
                <Trash2 className="h-4 w-4" />
              </Button>
            </div>
          ))}
          <Button
            type="button"
            variant="ghost"
            size="sm"
            disabled={question.choices.length >= 20}
            onClick={() => onChange({ choices: [...question.choices, ''] })}
          >
            <Plus className="mr-1.5 h-4 w-4" />
            Add choice
          </Button>
        </div>
      )}

      {isScale(question.kind) && (
        <div className="grid grid-cols-1 gap-2 sm:grid-cols-3">
          {question.kind === 'rating' && (
            <Select
              value={String(question.scale)}
              onValueChange={(v) => onChange({ scale: Number(v) })}
            >
              <SelectTrigger aria-label="Scale">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {RATING_SCALES.map((scale) => (
                  <SelectItem key={scale} value={String(scale)}>
                    1 to {scale}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          <Input
            aria-label="Low end label"
            placeholder="Low end label (optional)"
            value={question.lowerLabel}
            onChange={(e) => onChange({ lowerLabel: e.target.value })}
          />
          <Input
            aria-label="High end label"
            placeholder="High end label (optional)"
            value={question.upperLabel}
            onChange={(e) => onChange({ upperLabel: e.target.value })}
          />
        </div>
      )}

      <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
        <div className="flex items-center gap-2">
          <Switch
            id={`${base}-optional`}
            checked={question.optional}
            onCheckedChange={(checked) => onChange({ optional: checked })}
          />
          <Label htmlFor={`${base}-optional`}>Optional</Label>
        </div>
        <div className="flex items-center gap-2">
          <Label
            htmlFor={`${base}-id`}
            className="text-xs text-muted-foreground"
          >
            Answer id
          </Label>
          <Input
            id={`${base}-id`}
            className="h-8 w-[140px] font-mono text-xs"
            value={question.id}
            onChange={(e) => onChange({ id: e.target.value })}
          />
        </div>
      </div>
      {error && <p className="text-sm text-destructive">{error}</p>}
    </fieldset>
  )
}
