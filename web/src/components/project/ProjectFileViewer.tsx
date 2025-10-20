'use client'

import { useState, useEffect } from 'react'
import { Copy, Check, Download, X } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { cn } from '@/lib/utils'

interface ProjectFileViewerProps {
  filePath: string
  content: string
  language?: string
  className?: string
  onClose?: () => void
  readOnly?: boolean
}

export function ProjectFileViewer({
  filePath,
  content,
  language,
  className,
  onClose,
  readOnly = true,
}: ProjectFileViewerProps) {
  const [copied, setCopied] = useState(false)
  const [lines, setLines] = useState<string[]>([])

  useEffect(() => {
    setLines(content.split('\n'))
  }, [content])

  // Get language from file extension if not provided
  const getLanguage = () => {
    if (language) return language

    const ext = filePath.split('.').pop()?.toLowerCase()
    switch (ext) {
      case 'js':
      case 'jsx':
        return 'javascript'
      case 'ts':
      case 'tsx':
        return 'typescript'
      case 'py':
        return 'python'
      case 'rs':
        return 'rust'
      case 'go':
        return 'go'
      case 'java':
        return 'java'
      case 'cpp':
      case 'cc':
      case 'cxx':
        return 'cpp'
      case 'c':
        return 'c'
      case 'html':
        return 'html'
      case 'css':
        return 'css'
      case 'scss':
      case 'sass':
        return 'scss'
      case 'json':
        return 'json'
      case 'yaml':
      case 'yml':
        return 'yaml'
      case 'md':
        return 'markdown'
      case 'sh':
      case 'bash':
        return 'bash'
      case 'sql':
        return 'sql'
      case 'xml':
        return 'xml'
      default:
        return 'plaintext'
    }
  }

  // Copy content to clipboard
  const copyToClipboard = async () => {
    try {
      await navigator.clipboard.writeText(content)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch (err) {
      console.error('Failed to copy:', err)
    }
  }

  // Download file
  const downloadFile = () => {
    const blob = new Blob([content], { type: 'text/plain' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = filePath.split('/').pop() || 'file.txt'
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)
  }

  // Get file name from path
  const fileName = filePath.split('/').pop() || 'Untitled'

  return (
    <div className={cn('flex flex-col h-full bg-background', className)}>
      {/* Header */}
      <div className="shrink-0 border-b bg-muted/30 px-4 py-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <span className="text-sm font-medium">{fileName}</span>
            <span className="text-xs text-muted-foreground">
              {language || getLanguage()}
            </span>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7"
              onClick={copyToClipboard}
              title="Copy to clipboard"
            >
              {copied ? (
                <Check className="h-3.5 w-3.5 text-green-500" />
              ) : (
                <Copy className="h-3.5 w-3.5" />
              )}
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7"
              onClick={downloadFile}
              title="Download file"
            >
              <Download className="h-3.5 w-3.5" />
            </Button>
            {onClose && (
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7"
                onClick={onClose}
                title="Close"
              >
                <X className="h-3.5 w-3.5" />
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* Content */}
      <ScrollArea className="flex-1">
        <div className="relative">
          {/* Line numbers and content */}
          <div className="flex">
            {/* Line numbers */}
            <div className="shrink-0 select-none bg-muted/20 text-muted-foreground text-right pr-3 pl-4 py-4">
              {lines.map((_, index) => (
                <div key={index} className="text-xs leading-5 font-mono">
                  {index + 1}
                </div>
              ))}
            </div>

            {/* Code content */}
            <div className="flex-1 px-4 py-4 overflow-x-auto">
              <pre className="text-sm leading-5 font-mono">
                <code className={`language-${getLanguage()}`}>
                  {content || (
                    <span className="text-muted-foreground">Empty file</span>
                  )}
                </code>
              </pre>
            </div>
          </div>
        </div>
      </ScrollArea>

      {/* Footer with file info */}
      <div className="shrink-0 border-t bg-muted/30 px-4 py-1">
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <span>{lines.length} lines</span>
          <span>{content.length} characters</span>
          <span>{readOnly ? 'Read-only' : 'Editable'}</span>
        </div>
      </div>
    </div>
  )
}
