import { useState } from 'react'
import {
  Code,
  Eye,
  Terminal,
  FileText,
  GitBranch,
  PanelLeft,
  PanelLeftClose,
  Maximize2,
} from 'lucide-react'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Badge } from '@/components/ui/badge'
import {
  ResizablePanelGroup,
  ResizablePanel,
  ResizableHandle,
} from '@/components/ui/resizable'
import { DevProjectDto } from '@/api/client'
import { ProjectFileExplorer } from './ProjectFileExplorer'
import { MonacoCodeViewer } from './MonacoCodeViewer'
import { MultiTerminal } from './MultiTerminal'
import { GitManager } from './GitManager'

interface ProjectDevelopProps {
  project: {
    id: number
    name: string
    slug: string
    repo_owner: string
    repo_name: string
  }
  devProject: DevProjectDto
}

export function ProjectDevelop({ devProject }: ProjectDevelopProps) {
  const [selectedFile, setSelectedFile] = useState<string | null>(null)
  const [fileContent, setFileContent] = useState<string>('')
  const [editorMode, setEditorMode] = useState<'editor' | 'preview' | 'git'>(
    'editor'
  )
  const [activeTab, setActiveTab] = useState<'terminal' | 'logs'>('terminal')
  const [layout, setLayout] = useState<
    'vertical' | 'horizontal' | 'fullscreen'
  >('horizontal')

  const handleFileSelect = (filePath: string, content: string) => {
    setSelectedFile(filePath)
    setFileContent(content)
    setEditorMode('editor') // Switch to editor mode when file is selected
  }

  // Render editor/preview/git content
  const renderEditorContent = () => {
    if (editorMode === 'editor') {
      return selectedFile ? (
        <MonacoCodeViewer
          filePath={selectedFile}
          content={fileContent}
          className="h-full"
        />
      ) : (
        <div className="h-full flex items-center justify-center text-muted-foreground">
          <div className="text-center">
            <Code className="h-12 w-12 mx-auto mb-4 opacity-50" />
            <p>Select a file from the explorer to view</p>
          </div>
        </div>
      )
    } else if (editorMode === 'git') {
      return <GitManager devProject={devProject} className="h-full" />
    } else {
      return (
        <div className="h-full p-4">
          <div className="h-full border rounded-lg bg-white flex items-center justify-center">
            <div className="text-center text-muted-foreground">
              <Eye className="h-12 w-12 mx-auto mb-4 opacity-50" />
              <p>Preview will be shown here</p>
              <p className="text-sm">
                Connect to your development server to see live preview
              </p>
            </div>
          </div>
        </div>
      )
    }
  }

  // Render terminal/logs content
  const renderTerminalContent = () => (
    <div className="h-full flex flex-col">
      <div className="shrink-0 bg-muted border-b border-border">
        <div className="grid grid-cols-2">
          <button
            className={`px-4 py-2 text-sm flex items-center gap-2 justify-center hover:bg-accent ${activeTab === 'terminal' ? 'bg-accent text-foreground border-b-2 border-primary' : 'text-muted-foreground'}`}
            onClick={() => setActiveTab('terminal')}
          >
            <Terminal className="h-4 w-4" />
            Terminal
          </button>
          <button
            className={`px-4 py-2 text-sm flex items-center gap-2 justify-center hover:bg-accent ${activeTab === 'logs' ? 'bg-accent text-foreground border-b-2 border-primary' : 'text-muted-foreground'}`}
            onClick={() => setActiveTab('logs')}
          >
            <FileText className="h-4 w-4" />
            Logs
          </button>
        </div>
      </div>

      <div className="flex-1 relative">
        <div
          className={`absolute inset-0 ${activeTab === 'terminal' ? 'block' : 'hidden'}`}
        >
          <MultiTerminal devProject={devProject} className="h-full" />
        </div>
        <div
          className={`absolute inset-0 ${activeTab === 'logs' ? 'block' : 'hidden'}`}
        >
          <ScrollArea className="h-full p-4">
            <div className="space-y-2">
              <div className="text-sm text-muted-foreground">
                Development logs will appear here...
              </div>
              <div className="flex items-center gap-2 p-2 bg-muted/50 rounded text-sm">
                <Badge variant="outline">INFO</Badge>
                <span>Development server started on port 3000</span>
                <span className="text-xs text-muted-foreground ml-auto">
                  2s ago
                </span>
              </div>
              <div className="flex items-center gap-2 p-2 bg-muted/50 rounded text-sm">
                <Badge variant="outline">WARN</Badge>
                <span>Hot reload enabled</span>
                <span className="text-xs text-muted-foreground ml-auto">
                  1s ago
                </span>
              </div>
            </div>
          </ScrollArea>
        </div>
      </div>
    </div>
  )

  // Render editor header with mode toggles
  const renderEditorHeader = () => (
    <div className="border-b p-2 flex items-center gap-2">
      <Button
        variant={editorMode === 'editor' ? 'default' : 'outline'}
        size="sm"
        onClick={() => setEditorMode('editor')}
      >
        <Code className="h-4 w-4 mr-1" />
        Editor
      </Button>
      <Button
        variant={editorMode === 'preview' ? 'default' : 'outline'}
        size="sm"
        onClick={() => setEditorMode('preview')}
      >
        <Eye className="h-4 w-4 mr-1" />
        Preview
      </Button>
      <Button
        variant={editorMode === 'git' ? 'default' : 'outline'}
        size="sm"
        onClick={() => setEditorMode('git')}
      >
        <GitBranch className="h-4 w-4 mr-1" />
        Git
      </Button>
      {selectedFile && editorMode !== 'git' && (
        <Badge variant="secondary" className="ml-auto">
          {selectedFile}
        </Badge>
      )}

      {/* Layout Toggle Buttons */}
      <div className="flex items-center gap-1 ml-2 border-l pl-2">
        <Button
          variant={layout === 'vertical' ? 'default' : 'ghost'}
          size="sm"
          onClick={() => setLayout('vertical')}
          title="Vertical Layout (Editor top, Terminal bottom)"
        >
          <PanelLeft className="h-4 w-4" />
        </Button>
        <Button
          variant={layout === 'horizontal' ? 'default' : 'ghost'}
          size="sm"
          onClick={() => setLayout('horizontal')}
          title="Horizontal Layout (Editor and Terminal side by side)"
        >
          <PanelLeftClose className="h-4 w-4" />
        </Button>
        <Button
          variant={layout === 'fullscreen' ? 'default' : 'ghost'}
          size="sm"
          onClick={() => setLayout('fullscreen')}
          title="Fullscreen (No Terminal)"
        >
          <Maximize2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  )

  // Vertical Layout
  if (layout === 'vertical') {
    return (
      <div className="h-lvh overflow-visible">
        <ResizablePanelGroup direction="vertical" className="h-full">
          {/* Top Panel - Explorer and Editor */}
          <ResizablePanel defaultSize={50} minSize={30}>
            <ResizablePanelGroup direction="horizontal" className="h-full">
              {/* Left Sidebar - File Explorer */}
              <ResizablePanel defaultSize={20} minSize={15} maxSize={40}>
                <div className="h-full border-r bg-card">
                  <ProjectFileExplorer
                    devProject={devProject}
                    onFileSelect={handleFileSelect}
                    selectedFile={selectedFile}
                  />
                </div>
              </ResizablePanel>

              <ResizableHandle withHandle />

              {/* Right Side - Editor/Preview/Git */}
              <ResizablePanel defaultSize={80} minSize={60}>
                <div className="h-full flex flex-col">
                  {renderEditorHeader()}
                  <div className="flex-1 overflow-hidden">
                    {renderEditorContent()}
                  </div>
                </div>
              </ResizablePanel>
            </ResizablePanelGroup>
          </ResizablePanel>

          <ResizableHandle withHandle />

          {/* Bottom Panel - Terminal & Logs */}
          <ResizablePanel defaultSize={50} minSize={20}>
            <div className="h-full border-t overflow-visible">
              {renderTerminalContent()}
            </div>
          </ResizablePanel>
        </ResizablePanelGroup>
      </div>
    )
  }

  // Horizontal Layout
  if (layout === 'horizontal') {
    return (
      <div className="h-lvh overflow-visible">
        <ResizablePanelGroup direction="horizontal" className="h-full">
          {/* Left Sidebar - File Explorer */}
          <ResizablePanel defaultSize={20} minSize={15} maxSize={40}>
            <div className="h-full border-r bg-card">
              <ProjectFileExplorer
                devProject={devProject}
                onFileSelect={handleFileSelect}
                selectedFile={selectedFile}
              />
            </div>
          </ResizablePanel>

          <ResizableHandle withHandle />

          {/* Right Side - Editor and Terminal side by side */}
          <ResizablePanel defaultSize={80} minSize={60}>
            <div className="h-full flex flex-col">
              {renderEditorHeader()}
              <ResizablePanelGroup direction="horizontal" className="flex-1">
                {/* Editor Panel */}
                <ResizablePanel defaultSize={60} minSize={30}>
                  <div className="h-full overflow-hidden">
                    {renderEditorContent()}
                  </div>
                </ResizablePanel>

                <ResizableHandle withHandle />

                {/* Terminal Panel */}
                <ResizablePanel defaultSize={40} minSize={20}>
                  <div className="h-full border-l overflow-visible">
                    {renderTerminalContent()}
                  </div>
                </ResizablePanel>
              </ResizablePanelGroup>
            </div>
          </ResizablePanel>
        </ResizablePanelGroup>
      </div>
    )
  }

  // Fullscreen Layout (No Terminal)
  return (
    <div className="h-lvh overflow-visible">
      <ResizablePanelGroup direction="horizontal" className="h-full">
        {/* Left Sidebar - File Explorer */}
        <ResizablePanel defaultSize={20} minSize={15} maxSize={40}>
          <div className="h-full border-r bg-card">
            <ProjectFileExplorer
              devProject={devProject}
              onFileSelect={handleFileSelect}
              selectedFile={selectedFile}
            />
          </div>
        </ResizablePanel>

        <ResizableHandle withHandle />

        {/* Right Side - Editor/Preview/Git only */}
        <ResizablePanel defaultSize={80} minSize={60}>
          <div className="h-full flex flex-col">
            {renderEditorHeader()}
            <div className="flex-1 overflow-hidden">
              {renderEditorContent()}
            </div>
          </div>
        </ResizablePanel>
      </ResizablePanelGroup>
    </div>
  )
}
