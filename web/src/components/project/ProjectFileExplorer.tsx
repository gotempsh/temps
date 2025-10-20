'use client'

import { useState, useEffect } from 'react'
import {
  ChevronRight,
  ChevronDown,
  File,
  Folder,
  FolderOpen,
  RefreshCw,
  FileText,
  Code2,
  Image,
  FileJson,
  FileCode,
  Trash,
} from 'lucide-react'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { DevProjectDto, DirectoryEntry } from '@/api/client'
import {
  listDirectory,
  readFile,
  deleteFileOrDirectory,
} from '@/api/client/sdk.gen'
import { cn } from '@/lib/utils'

interface FileNode {
  name: string
  path: string
  type: 'file' | 'directory'
  children?: FileNode[]
  isLoaded?: boolean
  isExpanded?: boolean
  size?: number | null
  modifiedAt?: string | null
}

interface ProjectFileExplorerProps {
  devProject: DevProjectDto
  onFileSelect?: (filePath: string, content: string) => void
  selectedFile?: string | null
  className?: string
}

export function ProjectFileExplorer({
  devProject,
  onFileSelect,
  selectedFile,
  className,
}: ProjectFileExplorerProps) {
  const [fileTree, setFileTree] = useState<FileNode[]>([])
  const [loading, setLoading] = useState(false)
  const [loadingPaths, setLoadingPaths] = useState<Set<string>>(new Set())
  const [error, setError] = useState<string | null>(null)
  const [fileContent, setFileContent] = useState<Map<string, string>>(new Map())
  const [contextMenu, setContextMenu] = useState<{
    show: boolean
    x: number
    y: number
    filePath: string
    fileName: string
  } | null>(null)
  const [isExpanded, setIsExpanded] = useState(true)

  // Convert DirectoryEntry to FileNode
  const convertToFileNode = (entry: DirectoryEntry): FileNode => ({
    name: entry.name,
    path: entry.path,
    type: entry.is_directory ? 'directory' : 'file',
    children: entry.is_directory ? [] : undefined,
    isLoaded: false,
    isExpanded: false,
    size: entry.size,
    modifiedAt: entry.modified_at,
  })

  // Load root directory on mount
  useEffect(() => {
    loadDirectory('')
  }, [devProject.id])

  // Load directory contents
  const loadDirectory = async (path: string) => {
    setLoading(true)
    setLoadingPaths((prev) => new Set([...prev, path]))
    setError(null)

    try {
      const response = await listDirectory({
        path: { project_id: devProject.id },
        query: { path: path || '.' },
      })

      if (response.data) {
        const nodes = response.data.map(convertToFileNode)

        if (path === '' || path === '.') {
          // Root directory
          setFileTree(nodes)
        } else {
          // Subdirectory - update the tree
          setFileTree((prevTree) =>
            updateTreeWithChildren(prevTree, path, nodes)
          )
        }
      }
    } catch (err) {
      console.error('Failed to load directory:', err)
      setError('Failed to load directory contents')
    } finally {
      setLoading(false)
      setLoadingPaths((prev) => {
        const newSet = new Set(prev)
        newSet.delete(path)
        return newSet
      })
    }
  }

  // Update tree with loaded children
  const updateTreeWithChildren = (
    nodes: FileNode[],
    targetPath: string,
    children: FileNode[]
  ): FileNode[] => {
    return nodes.map((node) => {
      if (node.path === targetPath) {
        return {
          ...node,
          children,
          isLoaded: true,
          isExpanded: true,
        }
      }
      if (node.children) {
        return {
          ...node,
          children: updateTreeWithChildren(node.children, targetPath, children),
        }
      }
      return node
    })
  }

  // Toggle folder expansion
  const toggleFolder = async (node: FileNode) => {
    if (node.type !== 'directory') return

    if (!node.isLoaded) {
      // Load directory contents
      await loadDirectory(node.path)
    } else {
      // Just toggle expansion
      setFileTree((prevTree) => toggleNodeExpansion(prevTree, node.path))
    }
  }

  // Toggle node expansion state
  const toggleNodeExpansion = (
    nodes: FileNode[],
    targetPath: string
  ): FileNode[] => {
    return nodes.map((node) => {
      if (node.path === targetPath) {
        return { ...node, isExpanded: !node.isExpanded }
      }
      if (node.children) {
        return {
          ...node,
          children: toggleNodeExpansion(node.children, targetPath),
        }
      }
      return node
    })
  }

  // Handle file selection
  const handleFileSelect = async (node: FileNode) => {
    if (node.type !== 'file') return

    // Check if we already have the content cached
    if (fileContent.has(node.path)) {
      onFileSelect?.(node.path, fileContent.get(node.path)!)
      return
    }

    setLoadingPaths((prev) => new Set([...prev, node.path]))

    try {
      const response = await readFile({
        path: { project_id: devProject.id },
        query: { path: node.path },
      })

      if (response.data) {
        const content = response.data.content
        setFileContent((prev) => new Map(prev).set(node.path, content))
        onFileSelect?.(node.path, content)
      }
    } catch (err) {
      console.error('Failed to read file:', err)
      setError('Failed to read file contents')
    } finally {
      setLoadingPaths((prev) => {
        const newSet = new Set(prev)
        newSet.delete(node.path)
        return newSet
      })
    }
  }

  // Handle file/directory deletion
  const handleDelete = async (filePath: string) => {
    try {
      await deleteFileOrDirectory({
        path: { project_id: devProject.id },
        query: { path: filePath },
      })

      // Remove from file tree
      setFileTree((prevTree) => removeNodeFromTree(prevTree, filePath))

      // Remove from content cache
      setFileContent((prev) => {
        const newMap = new Map(prev)
        newMap.delete(filePath)
        return newMap
      })

      // Close context menu
      setContextMenu(null)
    } catch (err) {
      console.error('Failed to delete file/directory:', err)
      setError('Failed to delete file/directory')
    }
  }

  // Remove node from tree
  const removeNodeFromTree = (
    nodes: FileNode[],
    targetPath: string
  ): FileNode[] => {
    return nodes.filter((node) => {
      if (node.path === targetPath) {
        return false // Remove this node
      }
      if (node.children) {
        // Recursively check children
        node.children = removeNodeFromTree(node.children, targetPath)
      }
      return true
    })
  }

  // Handle right-click context menu
  const handleContextMenu = (e: React.MouseEvent, node: FileNode) => {
    e.preventDefault()
    setContextMenu({
      show: true,
      x: e.clientX,
      y: e.clientY,
      filePath: node.path,
      fileName: node.name,
    })
  }

  // Close context menu when clicking elsewhere
  const handleCloseContextMenu = () => {
    setContextMenu(null)
  }

  // Get file icon based on extension
  const getFileIcon = (fileName: string) => {
    const ext = fileName.split('.').pop()?.toLowerCase()

    switch (ext) {
      case 'js':
      case 'jsx':
      case 'ts':
      case 'tsx':
        return <FileCode className="h-4 w-4 text-blue-500" />
      case 'json':
        return <FileJson className="h-4 w-4 text-yellow-500" />
      case 'md':
      case 'txt':
        return <FileText className="h-4 w-4 text-muted-foreground" />
      case 'png':
      case 'jpg':
      case 'jpeg':
      case 'gif':
      case 'svg':
        return <Image className="h-4 w-4 text-green-500" />
      case 'html':
      case 'css':
      case 'scss':
        return <Code2 className="h-4 w-4 text-orange-500" />
      default:
        return <File className="h-4 w-4 text-muted-foreground" />
    }
  }

  // Render file tree recursively
  const renderTree = (nodes: FileNode[], level: number = 0) => {
    return nodes.map((node) => {
      const isDirectory = node.type === 'directory'
      const isExpanded = node.isExpanded
      const isLoading = loadingPaths.has(node.path)
      const isSelected = selectedFile === node.path

      return (
        <div key={node.path}>
          <div
            className={cn(
              'flex items-center gap-1 py-1 px-2 hover:bg-muted/50 cursor-pointer text-sm select-none',
              isSelected && 'bg-muted'
            )}
            style={{ paddingLeft: `${level * 12 + 8}px` }}
            onClick={() => {
              if (isDirectory) {
                toggleFolder(node)
              } else {
                handleFileSelect(node)
              }
            }}
            onContextMenu={(e) => handleContextMenu(e, node)}
          >
            {/* Chevron for directories */}
            {isDirectory ? (
              isLoading ? (
                <div className="w-4 h-4 animate-spin rounded-full border-2 border-primary border-t-transparent" />
              ) : isExpanded ? (
                <ChevronDown className="h-4 w-4 text-muted-foreground" />
              ) : (
                <ChevronRight className="h-4 w-4 text-muted-foreground" />
              )
            ) : (
              <div className="w-4" />
            )}

            {/* Icon */}
            {isDirectory ? (
              isExpanded ? (
                <FolderOpen className="h-4 w-4 text-blue-500" />
              ) : (
                <Folder className="h-4 w-4 text-blue-500" />
              )
            ) : (
              getFileIcon(node.name)
            )}

            {/* Name */}
            <span className="truncate flex-1 min-w-0" title={node.name}>
              {node.name}
            </span>

            {/* File size for files */}
            {!isDirectory && node.size && (
              <span className="text-xs text-muted-foreground ml-1 shrink-0">
                {formatFileSize(node.size)}
              </span>
            )}
          </div>

          {/* Render children if expanded */}
          {isDirectory && isExpanded && node.children && (
            <div>{renderTree(node.children, level + 1)}</div>
          )}
        </div>
      )
    })
  }

  // Format file size
  const formatFileSize = (bytes: number): string => {
    if (bytes < 1024) return `${bytes}B`
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`
    return `${(bytes / (1024 * 1024)).toFixed(1)}MB`
  }

  // Refresh the file tree
  const refresh = () => {
    setFileTree([])
    setFileContent(new Map())
    loadDirectory('')
  }

  return (
    <div
      className={cn('flex flex-col h-full', className)}
      onClick={handleCloseContextMenu}
    >
      {/* Header */}
      <div className="border-b">
        <div
          className="p-3 flex items-center justify-between cursor-pointer hover:bg-muted/50"
          onClick={(e) => {
            e.stopPropagation()
            setIsExpanded(!isExpanded)
          }}
        >
          <div className="flex items-center gap-2">
            {isExpanded ? (
              <ChevronDown className="h-4 w-4 text-muted-foreground" />
            ) : (
              <ChevronRight className="h-4 w-4 text-muted-foreground" />
            )}
            <h3 className="font-semibold text-sm">Explorer</h3>
          </div>
          <Button
            variant="ghost"
            size="icon"
            className="h-7 w-7"
            onClick={(e) => {
              e.stopPropagation()
              refresh()
            }}
            disabled={loading}
          >
            <RefreshCw
              className={cn('h-3.5 w-3.5', loading && 'animate-spin')}
            />
          </Button>
        </div>
      </div>

      {/* Content - only show when expanded */}
      {isExpanded && (
        <>
          {/* Error message */}
          {error && (
            <div className="px-3 py-2 text-sm text-destructive bg-destructive/10 border-b">
              {error}
            </div>
          )}

          {/* File tree */}
          <ScrollArea className="flex-1">
            <div className="p-1">
              {loading && fileTree.length === 0 ? (
                <div className="space-y-2 p-2">
                  {[...Array(5)].map((_, i) => (
                    <Skeleton key={i} className="h-6 w-full" />
                  ))}
                </div>
              ) : fileTree.length > 0 ? (
                renderTree(fileTree)
              ) : (
                <div className="p-4 text-center text-sm text-muted-foreground">
                  No files found
                </div>
              )}
            </div>
          </ScrollArea>
        </>
      )}

      {/* Context Menu */}
      {contextMenu && contextMenu.show && (
        <div
          className="fixed bg-popover border border-border rounded-md shadow-lg py-1 z-50 min-w-32"
          style={{
            left: contextMenu.x,
            top: contextMenu.y,
          }}
        >
          <button
            className="w-full px-3 py-2 text-left text-sm hover:bg-accent flex items-center gap-2"
            onClick={(e) => {
              e.stopPropagation()
              handleDelete(contextMenu.filePath)
            }}
          >
            <Trash className="h-4 w-4 text-destructive" />
            <span>Delete</span>
          </button>
        </div>
      )}
    </div>
  )
}
