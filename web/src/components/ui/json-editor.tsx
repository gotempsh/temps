import Editor from '@monaco-editor/react'
import * as React from 'react'

interface JsonEditorProps {
  value: string
  onChange: (value: string) => void
  onValidationChange?: (isValid: boolean, error?: string) => void
  placeholder?: string
  height?: string
  className?: string
}

const DEFAULT_JSON_VALUE = '{\n  "key": "value"\n}'

export function JsonEditor({
  value,
  onChange,
  onValidationChange,
  placeholder,
  height = '120px',
  className,
}: JsonEditorProps) {
  const [localValue, setLocalValue] = React.useState(
    value || DEFAULT_JSON_VALUE
  )
  const editorRef = React.useRef<any>(null)

  React.useEffect(() => {
    // Only update local value if it's different and not empty
    if (value && value !== localValue) {
      setLocalValue(value)
    }
  }, [value])

  const handleEditorChange = (newValue: string | undefined) => {
    const valueToUse = newValue || DEFAULT_JSON_VALUE
    setLocalValue(valueToUse)
    onChange(valueToUse)

    // Validate JSON
    if (onValidationChange) {
      try {
        const parsed = JSON.parse(valueToUse)

        // Must be a plain object
        if (
          typeof parsed !== 'object' ||
          parsed === null ||
          Array.isArray(parsed)
        ) {
          onValidationChange(false, 'Must be a plain JSON object')
          return
        }

        // Check all values are strings
        for (const [key, val] of Object.entries(parsed)) {
          if (typeof val !== 'string') {
            onValidationChange(false, `Value for key "${key}" must be a string`)
            return
          }
        }

        onValidationChange(true)
      } catch (e) {
        onValidationChange(false, 'Invalid JSON format')
      }
    }
  }

  const handleEditorDidMount = (editor: any) => {
    editorRef.current = editor

    // Set initial validation
    if (onValidationChange) {
      handleEditorChange(localValue)
    }
  }

  return (
    <div className={className}>
      <Editor
        height={height}
        defaultLanguage="json"
        value={localValue}
        onChange={handleEditorChange}
        onMount={handleEditorDidMount}
        options={{
          minimap: { enabled: false },
          lineNumbers: 'off',
          scrollBeyondLastLine: false,
          wordWrap: 'on',
          wrappingIndent: 'indent',
          fontSize: 13,
          tabSize: 2,
          insertSpaces: true,
          automaticLayout: true,
          padding: { top: 8, bottom: 8 },
          suggest: {
            showKeywords: false,
            showSnippets: false,
          },
          quickSuggestions: false,
          parameterHints: { enabled: false },
          folding: false,
          glyphMargin: false,
          lineDecorationsWidth: 0,
          lineNumbersMinChars: 0,
          renderLineHighlight: 'none',
          scrollbar: {
            vertical: 'auto',
            horizontal: 'auto',
            verticalScrollbarSize: 8,
            horizontalScrollbarSize: 8,
          },
        }}
        theme="vs-dark"
      />
    </div>
  )
}
