'use client'

import { useMemo, useState } from 'react'
import Editor from '@monaco-editor/react'
import {
  Copy,
  Check,
  Download,
  X,
  FileCode,
  Maximize2,
  Minimize2,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { useTheme } from 'next-themes'

interface MonacoCodeViewerProps {
  filePath: string
  content: string
  className?: string
  onClose?: () => void
}

export function MonacoCodeViewer({
  filePath,
  content,
  className,
  onClose,
}: MonacoCodeViewerProps) {
  const [copied, setCopied] = useState(false)
  const [isFullscreen, setIsFullscreen] = useState(false)
  const { theme, resolvedTheme } = useTheme()

  // Get language from file extension
  const getLanguage = () => {
    const ext = filePath.split('.').pop()?.toLowerCase()
    switch (ext) {
      case 'js':
        return 'javascript'
      case 'jsx':
        return 'javascript'
      case 'ts':
        return 'typescript'
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
      case 'cs':
        return 'csharp'
      case 'php':
        return 'php'
      case 'rb':
        return 'ruby'
      case 'swift':
        return 'swift'
      case 'kt':
        return 'kotlin'
      case 'scala':
        return 'scala'
      case 'r':
        return 'r'
      case 'html':
        return 'html'
      case 'css':
        return 'css'
      case 'scss':
      case 'sass':
        return 'scss'
      case 'less':
        return 'less'
      case 'json':
        return 'json'
      case 'yaml':
      case 'yml':
        return 'yaml'
      case 'xml':
        return 'xml'
      case 'md':
        return 'markdown'
      case 'sh':
      case 'bash':
        return 'shell'
      case 'ps1':
        return 'powershell'
      case 'sql':
        return 'sql'
      case 'dockerfile':
        return 'dockerfile'
      case 'graphql':
      case 'gql':
        return 'graphql'
      case 'vue':
        return 'vue'
      case 'lua':
        return 'lua'
      case 'dart':
        return 'dart'
      case 'elm':
        return 'elm'
      case 'clj':
        return 'clojure'
      case 'ex':
      case 'exs':
        return 'elixir'
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
  const vsCodeTheme = useMemo(() => {
    return resolvedTheme === 'dark' ? 'vs-dark' : 'vs-light'
  }, [resolvedTheme])

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
  const language = getLanguage()
  const lines = content.split('\n').length

  return (
    <div
      className={cn(
        'flex flex-col h-full bg-background',
        isFullscreen && 'fixed inset-0 z-50',
        className
      )}
    >
      {/* Header */}
      <div className="shrink-0 border-b border-border bg-card px-4 py-2">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-2">
            <FileCode className="h-4 w-4 text-muted-foreground" />
            <span
              className="text-sm font-medium text-foreground truncate max-w-[300px]"
              title={filePath}
            >
              {fileName}
            </span>
            <span className="text-xs text-muted-foreground px-2 py-0.5 bg-background rounded">
              {language}
            </span>
          </div>
          <div className="flex items-center gap-1">
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7 text-muted-foreground hover:text-foreground"
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
              className="h-7 w-7 text-muted-foreground hover:text-foreground"
              onClick={downloadFile}
              title="Download file"
            >
              <Download className="h-3.5 w-3.5" />
            </Button>
            <Button
              variant="ghost"
              size="icon"
              className="h-7 w-7 text-muted-foreground hover:text-foreground"
              onClick={() => setIsFullscreen(!isFullscreen)}
              title={isFullscreen ? 'Exit fullscreen' : 'Fullscreen'}
            >
              {isFullscreen ? (
                <Minimize2 className="h-3.5 w-3.5" />
              ) : (
                <Maximize2 className="h-3.5 w-3.5" />
              )}
            </Button>
            {onClose && (
              <Button
                variant="ghost"
                size="icon"
                className="h-7 w-7 text-muted-foreground hover:text-foreground"
                onClick={onClose}
                title="Close"
              >
                <X className="h-3.5 w-3.5" />
              </Button>
            )}
          </div>
        </div>
      </div>

      {/* Monaco Editor */}
      <div className="flex-1 overflow-hidden">
        <Editor
          height="100%"
          language={language}
          value={content}
          theme={vsCodeTheme}
          options={{
            readOnly: true,
            minimap: { enabled: lines > 100 },
            // fontSize: 13,
            // fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', Consolas, 'Courier New', monospace",
            fontLigatures: true,
            automaticLayout: true,
            wordWrap: 'off',
            scrollBeyondLastLine: false,
            renderWhitespace: 'selection',
            rulers: [80, 120],
            bracketPairColorization: {
              enabled: true,
            },
            padding: {
              top: 16,
              bottom: 16,
            },
            scrollbar: {
              vertical: 'auto',
              horizontal: 'auto',
              verticalScrollbarSize: 10,
              horizontalScrollbarSize: 10,
            },
            overviewRulerBorder: false,
            hideCursorInOverviewRuler: true,
            lineNumbers: 'on',
            glyphMargin: false,
            folding: true,
            lineDecorationsWidth: 0,
            lineNumbersMinChars: 3,
            renderLineHighlight: 'none',
            contextmenu: true,
            mouseWheelZoom: true,
            smoothScrolling: true,
            cursorSmoothCaretAnimation: 'on',
            roundedSelection: false,
            theme: vsCodeTheme,
          }}
          loading={
            <div className="flex items-center justify-center h-full bg-background">
              <div className="text-muted-foreground">Loading editor...</div>
            </div>
          }
        />
      </div>

      {/* Footer with file info */}
      <div className="shrink-0 border-t border-border bg-card px-4 py-1">
        <div className="flex items-center justify-between text-xs text-muted-foreground">
          <div className="flex items-center gap-4">
            <span>{lines} lines</span>
            <span>{content.length} characters</span>
          </div>
          <div className="flex items-center gap-4">
            <span>UTF-8</span>
            <span>Read Only</span>
          </div>
        </div>
      </div>
    </div>
  )
}
