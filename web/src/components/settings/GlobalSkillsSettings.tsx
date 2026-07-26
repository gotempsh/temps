import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { CreateActionButton } from '@/components/ui/create-action-button'
import { Card, CardContent } from '@/components/ui/card'
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
import { useMutation, useQuery } from '@tanstack/react-query'
import {
  ChevronRight,
  EllipsisVertical,
  FileCode,
  Loader2,
  Plus,
  Wand2,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { useNavigate } from 'react-router'
import { toast } from 'sonner'
import {
  createGlobalSkill,
  updateGlobalSkill,
} from '@/api/client/sdk.gen'
import {
  deleteGlobalSkillMutation,
  listGlobalSkillsOptions,
} from '@/api/client/@tanstack/react-query.gen'
import type { SkillDefinitionResponse as SkillDefinition } from '@/api/client/types.gen'

export function GlobalSkillsSettings() {
  usePageTitle('Skills')
  const navigate = useNavigate()

  const [skillToDelete, setSkillToDelete] = useState<string | null>(null)
  const [dialogOpen, setDialogOpen] = useState(false)
  const [editingSkill, setEditingSkill] = useState<SkillDefinition | null>(null)

  const {
    data: skillsData,
    isLoading,
    error,
    refetch,
  } = useQuery(listGlobalSkillsOptions())
  const skills = skillsData?.items

  const deleteMutation = useMutation({
    ...deleteGlobalSkillMutation(),
    onSuccess: () => {
      toast.success('Skill deleted')
      refetch()
      setSkillToDelete(null)
    },
    onError: () => toast.error('Failed to delete skill'),
  })

  const openCreate = () => {
    setEditingSkill(null)
    setDialogOpen(true)
  }

  const openEdit = (skill: SkillDefinition) => {
    setEditingSkill(skill)
    setDialogOpen(true)
  }

  return (
    <div>
      <div className="flex items-center justify-between mb-4">
        <div>
          <h2 className="text-lg font-semibold">Global Skills</h2>
          <p className="text-sm text-muted-foreground mt-1">
            Platform-wide skill definitions available to all projects. Skills
            are injected as{' '}
            <code className="text-xs bg-muted px-1 rounded">
              .claude/skills/
            </code>{' '}
            files in workflow sandboxes.
          </p>
        </div>
        <CreateActionButton
          onClick={openCreate}
          disabled={isLoading}
          label="Add Skill"
        />
      </div>

      {error && (
        <Card>
          <CardContent className="py-6">
            <p className="text-sm text-destructive">
              Failed to load skills.{' '}
              <button
                onClick={() => refetch()}
                className="underline hover:no-underline"
              >
                Retry
              </button>
            </p>
          </CardContent>
        </Card>
      )}

      {isLoading ? (
        <div className="flex items-center justify-center py-12">
          <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
        </div>
      ) : !error && skills && skills.length > 0 ? (
        <SkillsCompactRows
          skills={skills}
          onOpen={(slug) => navigate(`/skills/${slug}`)}
          onEdit={openEdit}
          onDelete={setSkillToDelete}
        />
      ) : !error ? (
        <Card>
          <CardContent className="flex flex-col items-center justify-center py-12">
            <FileCode className="h-12 w-12 text-muted-foreground/50 mb-4" />
            <h3 className="text-lg font-semibold mb-2">No global skills</h3>
            <p className="text-sm text-muted-foreground text-center mb-4 max-w-md">
              Global skills are available to all projects. Define common
              patterns, coding standards, or reusable instructions here.
            </p>
            <Button onClick={openCreate}>
              <Plus className="h-4 w-4 mr-2" />
              Create Your First Skill
            </Button>
          </CardContent>
        </Card>
      ) : null}

      <GlobalSkillDialog
        open={dialogOpen}
        onOpenChange={setDialogOpen}
        skill={editingSkill}
        onSuccess={() => {
          refetch()
          setDialogOpen(false)
        }}
      />

      <AlertDialog
        open={skillToDelete !== null}
        onOpenChange={() => setSkillToDelete(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete global skill?</AlertDialogTitle>
            <AlertDialogDescription>
              This will permanently delete this skill. All projects that
              reference it will lose access.
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              onClick={() => {
                if (skillToDelete)
                  deleteMutation.mutate({ path: { slug: skillToDelete } })
              }}
              className="bg-destructive text-destructive-foreground hover:bg-destructive/90"
            >
              Delete
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  )
}

// ── Shared row menu ──

interface SkillRowMenuProps {
  skill: SkillDefinition
  onView: () => void
  onEdit: () => void
  onDelete: () => void
}

function SkillRowMenu({ onView, onEdit, onDelete }: SkillRowMenuProps) {
  return (
    <div
      onClick={(e) => e.stopPropagation()}
      onPointerDown={(e) => e.stopPropagation()}
    >
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <Button variant="ghost" size="icon" className="h-8 w-8">
            <EllipsisVertical className="h-4 w-4" />
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem
            onSelect={(e) => {
              e.preventDefault()
              onView()
            }}
          >
            View details
          </DropdownMenuItem>
          <DropdownMenuItem
            onSelect={(e) => {
              e.preventDefault()
              onEdit()
            }}
          >
            Edit
          </DropdownMenuItem>
          <DropdownMenuSeparator />
          <DropdownMenuItem
            className="text-destructive"
            onSelect={(e) => {
              e.preventDefault()
              onDelete()
            }}
          >
            Delete
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  )
}

// ── Variant: Compact rows ──

interface SkillsListProps {
  skills: SkillDefinition[]
  onOpen: (slug: string) => void
  onEdit: (skill: SkillDefinition) => void
  onDelete: (slug: string) => void
}

function SkillsCompactRows({
  skills,
  onOpen,
  onEdit,
  onDelete,
}: SkillsListProps) {
  return (
    <div className="overflow-hidden rounded-lg border">
      <ul role="list" className="divide-y">
        {skills.map((skill) => (
          <li
            key={skill.id}
            role="button"
            tabIndex={0}
            onClick={() => onOpen(skill.slug)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault()
                onOpen(skill.slug)
              }
            }}
            className="flex cursor-pointer items-center gap-4 px-4 py-3 hover:bg-muted/40 transition-colors focus:outline-none focus:bg-muted/40"
          >
            <div className="flex size-9 shrink-0 items-center justify-center rounded-md bg-muted">
              <Wand2 className="size-4 text-muted-foreground" />
            </div>
            <div className="flex min-w-0 flex-1 items-center gap-3">
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <p className="truncate text-sm font-medium">{skill.name}</p>
                  <Badge variant="secondary" className="font-mono text-xs">
                    {skill.slug}
                  </Badge>
                </div>
                {skill.description && (
                  <p className="mt-0.5 truncate text-xs text-muted-foreground">
                    {skill.description}
                  </p>
                )}
              </div>
            </div>
            <SkillRowMenu
              skill={skill}
              onView={() => onOpen(skill.slug)}
              onEdit={() => onEdit(skill)}
              onDelete={() => onDelete(skill.slug)}
            />
            <ChevronRight className="size-4 shrink-0 text-muted-foreground/50" />
          </li>
        ))}
      </ul>
    </div>
  )
}

// ── Create / Edit Dialog ──

interface GlobalSkillDialogProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  skill: SkillDefinition | null
  onSuccess: () => void
}

function GlobalSkillDialog({
  open,
  onOpenChange,
  skill,
  onSuccess,
}: GlobalSkillDialogProps) {
  const isEdit = !!skill
  const [slug, setSlug] = useState('')
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [content, setContent] = useState('')
  const [isPending, setIsPending] = useState(false)

  useEffect(() => {
    if (open) {
      if (skill) {
        setSlug(skill.slug)
        setName(skill.name)
        setDescription(skill.description || '')
        setContent(skill.content)
      } else {
        setSlug('')
        setName('')
        setDescription('')
        setContent('')
      }
    }
  }, [open, skill])

  const handleNameChange = (value: string) => {
    setName(value)
    if (!isEdit) {
      setSlug(
        value
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, '-')
          .replace(/^-|-$/g, '')
      )
    }
  }

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim() || !slug.trim() || !content.trim()) return

    setIsPending(true)
    try {
      if (isEdit) {
        await updateGlobalSkill({
          path: { slug: skill!.slug },
          body: {
            name: name.trim(),
            description: description.trim() || undefined,
            content: content,
          },
          throwOnError: true,
        })
        toast.success('Skill updated')
      } else {
        await createGlobalSkill({
          body: {
            slug: slug.trim(),
            name: name.trim(),
            description: description.trim() || undefined,
            content: content,
          },
          throwOnError: true,
        })
        toast.success('Skill created')
      }
      onSuccess()
    } catch {
      toast.error(
        isEdit ? 'Failed to update skill' : 'Failed to create skill'
      )
    } finally {
      setIsPending(false)
    }
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl max-h-[90vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>
            {isEdit ? 'Edit Global Skill' : 'Create Global Skill'}
          </DialogTitle>
          <DialogDescription>
            {isEdit
              ? 'Update this global skill definition.'
              : 'Define a new global skill available to all projects.'}
          </DialogDescription>
        </DialogHeader>
        <form onSubmit={handleSubmit} className="space-y-4">
          <div className="grid grid-cols-1 sm:grid-cols-2 gap-4">
            <div className="space-y-2">
              <Label htmlFor="skill-name">Name</Label>
              <Input
                id="skill-name"
                value={name}
                onChange={(e) => handleNameChange(e.target.value)}
                placeholder="e.g. Code Review"
                required
              />
            </div>
            <div className="space-y-2">
              <Label htmlFor="skill-slug">Slug</Label>
              <Input
                id="skill-slug"
                value={slug}
                onChange={(e) => setSlug(e.target.value)}
                placeholder="e.g. code-review"
                disabled={isEdit}
                required
                className="font-mono"
              />
              {isEdit && (
                <p className="text-xs text-muted-foreground">
                  Slug cannot be changed after creation.
                </p>
              )}
            </div>
          </div>
          <div className="space-y-2">
            <Label htmlFor="skill-description">Description</Label>
            <Input
              id="skill-description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
              placeholder="Brief description of what this skill does"
            />
          </div>
          <div className="space-y-2">
            <Label htmlFor="skill-content">Content</Label>
            <p className="text-xs text-muted-foreground">
              The skill instructions in markdown. This becomes the SKILL.md
              file content.
            </p>
            <Textarea
              id="skill-content"
              value={content}
              onChange={(e) => setContent(e.target.value)}
              placeholder={`---\nname: my-skill\ndescription: What this skill does\n---\n\nInstructions for the AI when this skill is active...`}
              required
              className="font-mono text-sm min-h-[200px]"
              rows={12}
            />
          </div>
          <DialogFooter>
            <Button
              type="button"
              variant="outline"
              onClick={() => onOpenChange(false)}
            >
              Cancel
            </Button>
            <Button type="submit" disabled={isPending}>
              {isPending ? (
                <Loader2 className="h-4 w-4 animate-spin mr-2" />
              ) : null}
              {isPending
                ? isEdit
                  ? 'Saving...'
                  : 'Creating...'
                : isEdit
                  ? 'Save'
                  : 'Create Skill'}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  )
}
