import { ProjectResponse, CreateFunnelStep } from '@/api/client/types.gen'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from '@/components/ui/dialog'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { createFunnelMutation } from '@/api/client/@tanstack/react-query.gen'
import { useMutation, useQueryClient } from '@tanstack/react-query'
import { Plus, Trash2 } from 'lucide-react'
import * as React from 'react'

interface CreateFunnelDialogProps {
  project: ProjectResponse
  onSuccess: () => void
}

export function CreateFunnelDialog({
  project,
  onSuccess,
}: CreateFunnelDialogProps) {
  const [open, setOpen] = React.useState(false)
  const [formData, setFormData] = React.useState({
    name: '',
    description: '',
    steps: [
      { event_name: 'page_view', event_filter: null },
    ] as CreateFunnelStep[],
  })

  const queryClient = useQueryClient()

  const createFunnel = useMutation({
    ...createFunnelMutation(),
    meta: {
      errorTitle: 'Failed to create funnel',
    },
    onSuccess: (_data) => {
      queryClient.invalidateQueries({ queryKey: ['listFunnels'] })
      setOpen(false)
      setFormData({
        name: '',
        description: '',
        steps: [{ event_name: 'page_view', event_filter: null }],
      })
      onSuccess()
    },
  })

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault()
    if (
      !formData.name.trim() ||
      !formData.steps.some((step) => step.event_name.trim())
    ) {
      return
    }

    const validSteps = formData.steps.filter((step) => step.event_name.trim())

    createFunnel.mutate({
      path: {
        project_id: project.id,
      },
      body: {
        name: formData.name.trim(),
        description: formData.description.trim() || null,
        steps: validSteps,
      },
    })
  }

  const addStep = () => {
    setFormData((prev) => ({
      ...prev,
      steps: [...prev.steps, { event_name: '', event_filter: null }],
    }))
  }

  const updateStep = (index: number, eventName: string) => {
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.map((step, i) =>
        i === index ? { ...step, event_name: eventName } : step
      ),
    }))
  }

  const removeStep = (index: number) => {
    setFormData((prev) => ({
      ...prev,
      steps: prev.steps.filter((_, i) => i !== index),
    }))
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button>
          <Plus className="h-4 w-4 mr-2" />
          Create Funnel
        </Button>
      </DialogTrigger>
      <DialogContent className="max-w-2xl max-h-[80vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Create New Funnel</DialogTitle>
          <DialogDescription>
            Define a series of events to track user conversion through your
            application.
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="space-y-2">
            <Label htmlFor="name">Funnel Name</Label>
            <Input
              id="name"
              value={formData.name}
              onChange={(e) =>
                setFormData((prev) => ({ ...prev, name: e.target.value }))
              }
              placeholder="e.g., User Onboarding"
              required
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="description">Description (Optional)</Label>
            <Textarea
              id="description"
              value={formData.description}
              onChange={(e) =>
                setFormData((prev) => ({
                  ...prev,
                  description: e.target.value,
                }))
              }
              placeholder="Brief description of what this funnel tracks"
              rows={2}
            />
          </div>
          <div className="space-y-3">
            <div className="flex items-center justify-between">
              <Label>Funnel Steps</Label>
              <Button
                type="button"
                variant="outline"
                size="sm"
                onClick={addStep}
              >
                <Plus className="h-3 w-3 mr-1" />
                Add Step
              </Button>
            </div>
            {formData.steps.map((step, index) => (
              <div key={index} className="flex gap-2 items-center">
                <div className="flex-shrink-0 w-6 h-6 bg-primary text-primary-foreground rounded-full flex items-center justify-center text-xs font-medium">
                  {index + 1}
                </div>
                <Input
                  value={step.event_name}
                  onChange={(e) => updateStep(index, e.target.value)}
                  placeholder={
                    index === 0
                      ? 'Entry point (page_view)'
                      : 'Event name (e.g., button_click, form_submit)'
                  }
                  className="flex-1"
                  required
                  disabled={index === 0} // First step is always page_view and cannot be edited
                />
                {index === 0 ? (
                  <div className="w-10" /> // Spacer to align with remove buttons
                ) : (
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => removeStep(index)}
                  >
                    <Trash2 className="h-4 w-4" />
                  </Button>
                )}
              </div>
            ))}
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => setOpen(false)}
              disabled={createFunnel.isPending}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={createFunnel.isPending}>
              {createFunnel.isPending ? 'Creating...' : 'Create Funnel'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
