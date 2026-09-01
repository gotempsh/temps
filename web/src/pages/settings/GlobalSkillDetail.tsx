// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

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
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { usePageTitle } from '@/hooks/usePageTitle'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  ArrowLeft,
  Calendar,
  Loader2,
  Pencil,
  Save,
  Trash2,
  Wand2,
  X,
} from 'lucide-react'
import { useEffect, useState } from 'react'
import { Link, useNavigate, useParams } from 'react-router'
import { toast } from 'sonner'
import {
  deleteGlobalSkillMutation,
  listGlobalSkillsOptions,
  listGlobalSkillsQueryKey,
  updateGlobalSkillMutation,
} from '@/api/client/@tanstack/react-query.gen'

function formatDate(iso: string): string {
  try {
    return new Date(iso).toLocaleString()
  } catch {
    return iso
  }
}

export function GlobalSkillDetail() {
  const { slug } = useParams<{ slug: string }>()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  usePageTitle(`Skill: ${slug ?? ''}`)

  const {
    data: skillsData,
    isLoading,
    error,
    refetch,
  } = useQuery(listGlobalSkillsOptions())

  const skill = skillsData?.items.find((s) => s.slug === slug)

  const [isEditing, setIsEditing] = useState(false)
  const [name, setName] = useState('')
  const [description, setDescription] = useState('')
  const [content, setContent] = useState('')
  const [confirmDelete, setConfirmDelete] = useState(false)

  useEffect(() => {
    if (skill) {
      setName(skill.name)
      setDescription(skill.description ?? '')
      setContent(skill.content)
    }
  }, [skill])

  const updateMutation = useMutation({
    ...updateGlobalSkillMutation(),
    onSuccess: () => {
      toast.success('Skill updated')
      queryClient.invalidateQueries({ queryKey: listGlobalSkillsQueryKey() })
      setIsEditing(false)
    },
    onError: () => toast.error('Failed to update skill'),
  })

  const deleteMutation = useMutation({
    ...deleteGlobalSkillMutation(),
    onSuccess: () => {
      toast.success('Skill deleted')
      queryClient.invalidateQueries({ queryKey: listGlobalSkillsQueryKey() })
      navigate('/skills')
    },
    onError: () => toast.error('Failed to delete skill'),
  })

  if (isLoading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="h-6 w-6 animate-spin text-muted-foreground" />
      </div>
    )
  }

  if (error || !skill) {
    return (
      <div>
        <Link
          to="/skills"
          className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground mb-4"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to Skills
        </Link>
        <Card>
          <CardContent className="py-6">
            <p className="text-sm text-destructive">
              {error ? 'Failed to load skill.' : 'Skill not found.'}{' '}
              <button
                onClick={() => refetch()}
                className="underline hover:no-underline"
              >
                Retry
              </button>
            </p>
          </CardContent>
        </Card>
      </div>
    )
  }

  const handleCancel = () => {
    setName(skill.name)
    setDescription(skill.description ?? '')
    setContent(skill.content)
    setIsEditing(false)
  }

  const handleSave = (e: React.FormEvent) => {
    e.preventDefault()
    if (!name.trim() || !content.trim()) return
    updateMutation.mutate({
      path: { slug: slug! },
      body: {
        name: name.trim(),
        description: description.trim() || undefined,
        content,
      },
    })
  }

  return (
    <div>
      <Link
        to="/skills"
        className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground mb-4"
      >
        <ArrowLeft className="h-4 w-4" />
        Back to Skills
      </Link>

      <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:justify-between mb-6">
        <div className="flex items-start gap-3 flex-1 min-w-0">
          <div className="mt-1">
            <Wand2 className="h-6 w-6 text-muted-foreground" />
          </div>
          <div className="flex-1 min-w-0">
            <div className="flex items-center gap-2 flex-wrap mb-1">
              <h1 className="text-xl font-semibold truncate">{skill.name}</h1>
              <Badge variant="secondary" className="font-mono text-xs">
                {skill.slug}
              </Badge>
            </div>
            {skill.description && (
              <p className="text-sm text-muted-foreground">
                {skill.description}
              </p>
            )}
          </div>
        </div>
        <div className="flex gap-2">
          {isEditing ? (
            <>
              <Button
                variant="outline"
                onClick={handleCancel}
                disabled={updateMutation.isPending}
              >
                <X className="h-4 w-4 mr-2" />
                Cancel
              </Button>
              <Button
                onClick={handleSave}
                disabled={
                  updateMutation.isPending || !name.trim() || !content.trim()
                }
              >
                {updateMutation.isPending ? (
                  <Loader2 className="h-4 w-4 animate-spin mr-2" />
                ) : (
                  <Save className="h-4 w-4 mr-2" />
                )}
                Save
              </Button>
            </>
          ) : (
            <>
              <Button variant="outline" onClick={() => setIsEditing(true)}>
                <Pencil className="h-4 w-4 mr-2" />
                Edit
              </Button>
              <Button
                variant="outline"
                className="text-destructive hover:text-destructive"
                onClick={() => setConfirmDelete(true)}
              >
                <Trash2 className="h-4 w-4 mr-2" />
                Delete
              </Button>
            </>
          )}
        </div>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-4">
        <Card className="lg:col-span-2">
          <CardHeader>
            <CardTitle className="text-base">
              {isEditing ? 'Edit Skill' : 'Skill Content'}
            </CardTitle>
          </CardHeader>
          <CardContent>
            {isEditing ? (
              <form onSubmit={handleSave} className="space-y-4">
                <div className="space-y-2">
                  <Label htmlFor="skill-name">Name</Label>
                  <Input
                    id="skill-name"
                    value={name}
                    onChange={(e) => setName(e.target.value)}
                    required
                  />
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
                    The skill instructions in markdown. Becomes the SKILL.md
                    file content.
                  </p>
                  <Textarea
                    id="skill-content"
                    value={content}
                    onChange={(e) => setContent(e.target.value)}
                    required
                    className="font-mono text-sm min-h-[320px]"
                    rows={18}
                  />
                </div>
              </form>
            ) : (
              <div className="rounded-md border bg-muted/50 p-3">
                <pre className="text-xs whitespace-pre-wrap font-mono overflow-x-auto">
                  {skill.content}
                </pre>
              </div>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle className="text-base">Details</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1">
                Slug
              </div>
              <code className="text-xs bg-muted px-1.5 py-0.5 rounded font-mono">
                {skill.slug}
              </code>
            </div>
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1">
                Scope
              </div>
              <Badge variant="outline" className="text-xs">
                {skill.project_id === null ? 'Global' : `Project ${skill.project_id}`}
              </Badge>
            </div>
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1 flex items-center gap-1">
                <Calendar className="h-3 w-3" /> Created
              </div>
              <div className="text-xs">{formatDate(skill.created_at)}</div>
            </div>
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1 flex items-center gap-1">
                <Calendar className="h-3 w-3" /> Updated
              </div>
              <div className="text-xs">{formatDate(skill.updated_at)}</div>
            </div>
            <div>
              <div className="text-xs font-medium text-muted-foreground mb-1">
                Content size
              </div>
              <div className="text-xs">{skill.content.length} chars</div>
            </div>
          </CardContent>
        </Card>
      </div>

      <AlertDialog open={confirmDelete} onOpenChange={setConfirmDelete}>
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
              onClick={() => deleteMutation.mutate({ path: { slug: slug! } })}
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
