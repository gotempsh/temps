'use client'

import { useState, useEffect, useCallback } from 'react'
import {
  Plus,
  Minus,
  RotateCcw,
  RefreshCw,
  FileText,
  AlertCircle,
  GitCommitHorizontal,
  ArrowDown,
  ArrowUp,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Textarea } from '@/components/ui/textarea'
import { ScrollArea } from '@/components/ui/scroll-area'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { DevProjectDto, GitStatus } from '@/api/client'
import {
  getGitStatus,
  gitAdd,
  gitCommit,
  gitPull,
  gitPush,
  getBranches,
  gitUnstage,
  deleteFileOrDirectory,
} from '@/api/client/sdk.gen'
import { cn } from '@/lib/utils'

interface GitManagerProps {
  devProject: DevProjectDto
  className?: string
}

export function GitManager({ devProject, className }: GitManagerProps) {
  const [gitStatus, setGitStatus] = useState<GitStatus | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [commitMessage, setCommitMessage] = useState('')
  const [_newBranchName, _setNewBranchName] = useState('')
  const [showCommitDialog, setShowCommitDialog] = useState(false)
  const [selectedFiles, setSelectedFiles] = useState<Set<string>>(new Set())
  const [lastClickedFile, setLastClickedFile] = useState<string | null>(null)
  const [operationLoading, setOperationLoading] = useState<string | null>(null)

  // Load git status
  const loadGitStatus = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const response = await getGitStatus({
        path: { project_id: devProject.id },
      })
      if (response.data) {
        setGitStatus(response.data)
        // Reset selection state when git status changes
        setSelectedFiles(new Set())
        setLastClickedFile(null)
      }
    } catch (err) {
      console.error('Failed to load git status:', err)
      setError('Failed to load git status')
    } finally {
      setLoading(false)
    }
  }, [devProject.id])

  // Load branches
  const loadBranches = useCallback(async () => {
    try {
      const response = await getBranches({
        path: { project_id: devProject.id },
      })
      if (response.data) {
        // branches state removed as unused
      }
    } catch (err) {
      console.error('Failed to load branches:', err)
    }
  }, [devProject.id])

  // Load data on mount
  useEffect(() => {
    loadGitStatus()
    loadBranches()
  }, [loadGitStatus, loadBranches])

  // Stage files
  const stageFiles = async (files: string[]) => {
    setOperationLoading('staging')
    try {
      await gitAdd({
        path: { project_id: devProject.id },
        body: { files: files },
      })
      await loadGitStatus()
      setSelectedFiles(new Set())
    } catch (err) {
      console.error('Failed to stage files:', err)
      setError('Failed to stage files')
    } finally {
      setOperationLoading(null)
    }
  }

  // Unstage files
  const unstageFiles = async (files: string[]) => {
    setOperationLoading('unstaging')
    try {
      await gitUnstage({
        path: { project_id: devProject.id },
        body: { files: files },
      })
      await loadGitStatus()
    } catch (err) {
      console.error('Failed to unstage files:', err)
      setError('Failed to unstage files')
    } finally {
      setOperationLoading(null)
    }
  }

  // Commit changes
  const commitChanges = async () => {
    if (!commitMessage.trim()) return

    setOperationLoading('committing')
    try {
      await gitCommit({
        path: { project_id: devProject.id },
        body: { message: commitMessage },
      })
      setCommitMessage('')
      setShowCommitDialog(false)
      await loadGitStatus()
    } catch (err) {
      console.error('Failed to commit:', err)
      setError('Failed to commit changes')
    } finally {
      setOperationLoading(null)
    }
  }

  // Pull changes
  const pullChanges = async () => {
    setOperationLoading('pulling')
    try {
      await gitPull({
        path: { project_id: devProject.id },
      })
      await loadGitStatus()
    } catch (err) {
      console.error('Failed to pull:', err)
      setError('Failed to pull changes')
    } finally {
      setOperationLoading(null)
    }
  }

  // Push changes
  const pushChanges = async () => {
    setOperationLoading('pushing')
    try {
      await gitPush({
        path: { project_id: devProject.id },
      })
      await loadGitStatus()
    } catch (err) {
      console.error('Failed to push:', err)
      setError('Failed to push changes')
    } finally {
      setOperationLoading(null)
    }
  }

  // Delete selected files
  const deleteSelectedFiles = async () => {
    if (selectedFiles.size === 0) return

    setOperationLoading('deleting')
    try {
      // Delete files from filesystem
      for (const filePath of selectedFiles) {
        await deleteFileOrDirectory({
          path: { project_id: devProject.id },
          query: { path: filePath },
        })
      }

      // Clear selection and refresh git status
      setSelectedFiles(new Set())
      await loadGitStatus()
    } catch (err) {
      console.error('Failed to delete files:', err)
      setError('Failed to delete selected files')
    } finally {
      setOperationLoading(null)
    }
  }

  // Get all files in display order
  const getAllFiles = () => {
    if (!gitStatus) return []
    return [...gitStatus.modified, ...gitStatus.untracked]
  }

  // Handle file selection with click, alt+click, and shift+click
  const handleFileSelection = (filePath: string, event: React.MouseEvent) => {
    const newSelected = new Set(selectedFiles)
    const allFiles = getAllFiles()

    if (event.shiftKey && lastClickedFile) {
      // Shift+click: select range from last clicked file to current file
      const startIndex = allFiles.findIndex((f) => f.path === lastClickedFile)
      const endIndex = allFiles.findIndex((f) => f.path === filePath)

      if (startIndex !== -1 && endIndex !== -1) {
        const start = Math.min(startIndex, endIndex)
        const end = Math.max(startIndex, endIndex)

        // Select all files in the range
        for (let i = start; i <= end; i++) {
          newSelected.add(allFiles[i].path)
        }
      }
      setSelectedFiles(newSelected)
      setLastClickedFile(filePath)
    } else if (event.altKey || event.metaKey) {
      // Alt+click: toggle individual file selection
      if (newSelected.has(filePath)) {
        newSelected.delete(filePath)
      } else {
        newSelected.add(filePath)
      }
      setSelectedFiles(newSelected)
      setLastClickedFile(filePath)
    } else {
      // Regular click: clear current selection and select only this file
      newSelected.clear()
      newSelected.add(filePath)
      setSelectedFiles(newSelected)
      setLastClickedFile(filePath)
    }
  }

  // Render file status icon
  const getFileStatusIcon = (status: string) => {
    switch (status.toLowerCase()) {
      case 'modified':
      case 'm':
        return <FileText className="h-4 w-4 text-orange-500" />
      case 'added':
      case 'a':
        return <Plus className="h-4 w-4 text-green-500" />
      case 'deleted':
      case 'd':
        return <Minus className="h-4 w-4 text-red-500" />
      case 'renamed':
      case 'r':
        return <RotateCcw className="h-4 w-4 text-blue-500" />
      case 'untracked':
      case '??':
        return <AlertCircle className="h-4 w-4 text-muted-foreground" />
      default:
        return <FileText className="h-4 w-4 text-muted-foreground" />
    }
  }

  if (loading && !gitStatus) {
    return (
      <div className={cn('flex items-center justify-center h-full', className)}>
        <div className="text-center">
          <RefreshCw className="h-8 w-8 animate-spin mx-auto mb-2 text-muted-foreground" />
          <p className="text-sm text-muted-foreground">Loading git status...</p>
        </div>
      </div>
    )
  }

  return (
    <div
      className={cn(
        'flex flex-col h-full bg-card text-card-foreground',
        className
      )}
    >
      {/* Header */}
      <div className="shrink-0 px-3 py-2 border-b border-border">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-xs font-semibold text-foreground uppercase tracking-wide">
              Source Control
            </span>
            {gitStatus && (
              <span className="text-xs text-muted-foreground">
                ({gitStatus.branch})
              </span>
            )}
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon"
              onClick={() => {
                loadGitStatus()
                loadBranches()
              }}
              disabled={loading}
              className="h-6 w-6 hover:bg-accent text-foreground"
              title="Refresh"
            >
              <RefreshCw
                className={cn('h-3.5 w-3.5', loading && 'animate-spin')}
              />
            </Button>
          </div>
        </div>
      </div>

      {error && (
        <div className="bg-destructive/10 border-b border-destructive px-4 py-2">
          <p className="text-sm text-destructive">{error}</p>
        </div>
      )}

      {gitStatus && (
        <div className="flex-1 overflow-hidden">
          {/* Action buttons */}
          <div className="px-3 py-2 border-b border-border">
            <div className="flex items-center gap-1">
              <Button
                size="sm"
                onClick={pullChanges}
                disabled={operationLoading === 'pulling'}
                className="h-7 px-2 bg-primary hover:bg-primary/90 text-primary-foreground text-xs"
                title="Pull"
              >
                {operationLoading === 'pulling' ? (
                  <RefreshCw className="h-3 w-3 animate-spin" />
                ) : (
                  <ArrowDown className="h-3 w-3" />
                )}
              </Button>
              <Button
                size="sm"
                onClick={pushChanges}
                disabled={
                  operationLoading === 'pushing' ||
                  (gitStatus.staged.length === 0 && gitStatus.ahead === 0)
                }
                className="h-7 px-2 bg-primary hover:bg-primary/90 text-primary-foreground text-xs"
                title="Push"
              >
                {operationLoading === 'pushing' ? (
                  <RefreshCw className="h-3 w-3 animate-spin" />
                ) : (
                  <ArrowUp className="h-3 w-3" />
                )}
              </Button>
              {gitStatus.staged.length > 0 && (
                <Button
                  size="sm"
                  onClick={() => setShowCommitDialog(true)}
                  className="h-7 px-2 bg-primary hover:bg-primary/90 text-primary-foreground text-xs"
                  title="Commit"
                >
                  <GitCommitHorizontal className="h-3 w-3" />
                </Button>
              )}
            </div>
          </div>

          <ScrollArea className="flex-1">
            <div className="p-3 space-y-4">
              {/* Staged Changes */}
              {gitStatus.staged.length > 0 && (
                <div>
                  <div className="flex items-center justify-between px-1 py-1">
                    <h3 className="text-xs font-medium text-foreground uppercase tracking-wide">
                      Staged Changes
                    </h3>
                    <div className="flex items-center gap-2">
                      <span className="text-xs text-muted-foreground">
                        {gitStatus.staged.length}
                      </span>
                      <Button
                        size="icon"
                        variant="ghost"
                        onClick={() =>
                          unstageFiles(gitStatus.staged.map((f) => f.path))
                        }
                        disabled={operationLoading === 'unstaging'}
                        className="h-4 w-4 hover:bg-muted text-foreground"
                        title="Unstage all files"
                      >
                        <Minus className="h-3 w-3" />
                      </Button>
                    </div>
                  </div>
                  <div className="space-y-0">
                    {gitStatus.staged.map((file) => (
                      <div
                        key={file.path}
                        className="flex items-center gap-2 px-1 py-1 hover:bg-accent rounded text-sm group"
                      >
                        {getFileStatusIcon(file.status)}
                        <span
                          className="flex-1 truncate text-foreground"
                          title={file.path}
                        >
                          {file.path}
                        </span>
                        <span className="text-xs text-primary font-mono">
                          {file.status}
                        </span>
                        <Button
                          size="icon"
                          variant="ghost"
                          onClick={(e) => {
                            e.stopPropagation()
                            unstageFiles([file.path])
                          }}
                          className="h-5 w-5 opacity-0 group-hover:opacity-100 hover:bg-muted text-foreground"
                          disabled={operationLoading === 'unstaging'}
                          title="Unstage file"
                        >
                          <Minus className="h-3 w-3" />
                        </Button>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              {/* Changes */}
              {(gitStatus.modified.length > 0 ||
                gitStatus.untracked.length > 0) && (
                <div>
                  <div className="px-1 py-1">
                    <div className="flex items-center justify-between">
                      <h3 className="text-xs font-medium text-foreground uppercase tracking-wide">
                        Changes
                      </h3>
                      <span className="text-xs text-muted-foreground">
                        {gitStatus.modified.length + gitStatus.untracked.length}
                      </span>
                    </div>
                    {gitStatus.modified.length + gitStatus.untracked.length >
                      1 && (
                      <div className="text-xs text-muted-foreground mt-1">
                        Click, Alt+click, Shift+click to select
                      </div>
                    )}
                  </div>
                  <div className="space-y-0">
                    {[...gitStatus.modified, ...gitStatus.untracked].map(
                      (file) => (
                        <div
                          key={file.path}
                          className={cn(
                            'flex items-center gap-2 px-1 py-1 hover:bg-accent rounded text-sm cursor-pointer group',
                            selectedFiles.has(file.path) && 'bg-primary/20'
                          )}
                          onClick={(e) => handleFileSelection(file.path, e)}
                        >
                          {getFileStatusIcon(file.status)}
                          <span
                            className="flex-1 truncate text-foreground"
                            title={file.path}
                          >
                            {file.path}
                          </span>
                          <span className="text-xs text-primary font-mono">
                            {file.status}
                          </span>
                          <Button
                            size="icon"
                            variant="ghost"
                            onClick={(e) => {
                              e.stopPropagation()
                              stageFiles([file.path])
                            }}
                            className="h-5 w-5 opacity-0 group-hover:opacity-100 hover:bg-muted text-foreground"
                            title="Stage changes"
                          >
                            <Plus className="h-3 w-3" />
                          </Button>
                        </div>
                      )
                    )}
                  </div>
                  {selectedFiles.size > 0 && (
                    <div className="px-1 py-2 space-y-1">
                      <Button
                        size="sm"
                        onClick={() => stageFiles(Array.from(selectedFiles))}
                        disabled={operationLoading === 'staging'}
                        className="h-6 px-2 bg-primary hover:bg-primary/90 text-primary-foreground text-xs w-full"
                      >
                        {operationLoading === 'staging' ? (
                          <RefreshCw className="h-3 w-3 animate-spin mr-1" />
                        ) : (
                          <Plus className="h-3 w-3 mr-1" />
                        )}
                        Stage Selected ({selectedFiles.size})
                      </Button>
                      <Button
                        size="sm"
                        onClick={deleteSelectedFiles}
                        disabled={operationLoading === 'deleting'}
                        variant="destructive"
                        className="h-6 px-2 text-xs w-full"
                      >
                        {operationLoading === 'deleting' ? (
                          <RefreshCw className="h-3 w-3 animate-spin mr-1" />
                        ) : (
                          <Minus className="h-3 w-3 mr-1" />
                        )}
                        Delete Selected ({selectedFiles.size})
                      </Button>
                    </div>
                  )}
                </div>
              )}

              {/* Commit input */}
              {gitStatus.staged.length > 0 && (
                <div className="border-t border-border pt-3">
                  <div className="px-1">
                    <Textarea
                      placeholder="Message (press Ctrl+Enter to commit)"
                      value={commitMessage}
                      onChange={(e) => setCommitMessage(e.target.value)}
                      className="min-h-16 resize-none"
                      onKeyDown={(e) => {
                        if (
                          e.ctrlKey &&
                          e.key === 'Enter' &&
                          commitMessage.trim()
                        ) {
                          commitChanges()
                        }
                      }}
                    />
                    <div className="flex justify-between items-center mt-2">
                      <span className="text-xs text-muted-foreground">
                        {gitStatus.staged.length} staged change
                        {gitStatus.staged.length !== 1 ? 's' : ''}
                      </span>
                      <Button
                        size="sm"
                        onClick={commitChanges}
                        disabled={
                          !commitMessage.trim() ||
                          operationLoading === 'committing'
                        }
                        className="h-6 px-3 text-xs"
                      >
                        {operationLoading === 'committing' ? (
                          <RefreshCw className="h-3 w-3 animate-spin mr-1" />
                        ) : (
                          <GitCommitHorizontal className="h-3 w-3 mr-1" />
                        )}
                        Commit
                      </Button>
                    </div>
                  </div>
                </div>
              )}
            </div>
          </ScrollArea>
        </div>
      )}

      {/* Commit Dialog */}
      <Dialog open={showCommitDialog} onOpenChange={setShowCommitDialog}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Commit Changes</DialogTitle>
            <DialogDescription>
              Write a commit message for your staged changes
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <Textarea
              placeholder="Commit message..."
              value={commitMessage}
              onChange={(e) => setCommitMessage(e.target.value)}
              className="min-h-20"
            />
          </div>
          <DialogFooter>
            <Button
              variant="outline"
              onClick={() => setShowCommitDialog(false)}
            >
              Cancel
            </Button>
            <Button
              onClick={commitChanges}
              disabled={
                !commitMessage.trim() || operationLoading === 'committing'
              }
            >
              {operationLoading === 'committing' ? (
                <RefreshCw className="h-4 w-4 animate-spin mr-2" />
              ) : (
                <GitCommitHorizontal className="h-4 w-4 mr-2" />
              )}
              Commit
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}
