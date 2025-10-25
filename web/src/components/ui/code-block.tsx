import { useState } from 'react'
import { Button } from '@/components/ui/button'
import { Check, Copy, Hash, WrapText } from 'lucide-react'
import { cn } from '@/lib/utils'

interface CodeBlockProps {
  code: string
  language?:
    | 'bash'
    | 'yaml'
    | 'json'
    | 'javascript'
    | 'typescript'
    | 'shell'
    | 'text'
    | 'python'
  className?: string
  showCopy?: boolean
  title?: string
  defaultWrap?: boolean
  defaultShowLineNumbers?: boolean
  disableWrapToggle?: boolean
}

export function CodeBlock({
  code,
  language = 'text',
  className,
  showCopy = true,
  title,
  defaultWrap = false,
  defaultShowLineNumbers = false,
  disableWrapToggle = false,
}: CodeBlockProps) {
  const [copied, setCopied] = useState(false)
  const [wrapLines, setWrapLines] = useState(defaultWrap)
  const [showLineNumbers, setShowLineNumbers] = useState(defaultShowLineNumbers)

  const handleCopy = async () => {
    await navigator.clipboard.writeText(code)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }

  // Simple syntax highlighting - returns React elements instead of HTML strings
  const renderHighlightedCode = (code: string, lang: string) => {
    const lines = code.split('\n')

    if (lang === 'bash' || lang === 'shell') {
      return lines.map((line, i) => (
        <div key={i} className="flex">
          {showLineNumbers && (
            <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
              {i + 1}
            </span>
          )}
          <div className="flex-1">
            {line.trim().startsWith('#') ? (
              <span className="text-muted-foreground opacity-70 italic">
                {line}
              </span>
            ) : (
              <>
                {line.split(' ').map((word, j) => {
                  // Commands
                  if (
                    j === 0 &&
                    [
                      'npm',
                      'yarn',
                      'pnpm',
                      'bun',
                      'curl',
                      'brew',
                      'sudo',
                      'chmod',
                      'cloudflared',
                      'systemctl',
                      'mkdir',
                      'cd',
                      'ls',
                      'echo',
                      'export',
                      'cat',
                      'mv',
                      'cp',
                    ].includes(word)
                  ) {
                    return (
                      <span
                        key={j}
                        className="text-blue-600 dark:text-blue-400 font-semibold"
                      >
                        {word}{' '}
                      </span>
                    )
                  }
                  // Flags
                  if (word.startsWith('-')) {
                    return (
                      <span
                        key={j}
                        className="text-orange-600 dark:text-orange-400"
                      >
                        {word}{' '}
                      </span>
                    )
                  }
                  // Environment variables
                  if (word.includes('=') && !word.startsWith('-')) {
                    const [key, value] = word.split('=')
                    return (
                      <span key={j}>
                        <span className="text-purple-600 dark:text-purple-400">
                          {key}
                        </span>
                        <span className="text-muted-foreground">=</span>
                        <span className="text-green-600 dark:text-green-400">
                          {value}
                        </span>
                        <span> </span>
                      </span>
                    )
                  }
                  return <span key={j}>{word} </span>
                })}
              </>
            )}
          </div>
        </div>
      ))
    }

    if (lang === 'python') {
      const keywords = [
        'import',
        'from',
        'def',
        'class',
        'return',
        'if',
        'elif',
        'else',
        'for',
        'while',
        'try',
        'except',
        'finally',
        'with',
        'as',
        'pass',
        'break',
        'continue',
        'global',
        'nonlocal',
        'lambda',
        'yield',
        'raise',
        'del',
        'assert',
        'and',
        'or',
        'not',
        'in',
        'is',
      ]
      const builtins = [
        'True',
        'False',
        'None',
        'print',
        'len',
        'range',
        'str',
        'int',
        'float',
        'list',
        'dict',
        'tuple',
        'set',
        'open',
        'file',
        'input',
        'type',
        'super',
        'self',
      ]

      return lines.map((line, lineIdx) => {
        // Comments
        if (line.trim().startsWith('#')) {
          return (
            <div key={lineIdx} className="flex">
              {showLineNumbers && (
                <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                  {lineIdx + 1}
                </span>
              )}
              <div className="flex-1 text-muted-foreground opacity-70 italic">
                {line}
              </div>
            </div>
          )
        }

        // Process each line with a simple tokenizer
        const tokens: React.ReactNode[] = []
        let current = ''
        let inString = false
        let stringChar = ''

        for (let i = 0; i < line.length; i++) {
          const char = line[i]

          // Handle strings
          if ((char === '"' || char === "'") && !inString) {
            if (current) {
              tokens.push(renderPythonToken(current, keywords, builtins))
              current = ''
            }
            inString = true
            stringChar = char
            current = char
          } else if (char === stringChar && inString) {
            current += char
            tokens.push(
              <span className="text-green-600 dark:text-green-400">
                {current}
              </span>
            )
            current = ''
            inString = false
            stringChar = ''
          } else if (inString) {
            current += char
          } else if (
            char === ' ' ||
            char === '(' ||
            char === ')' ||
            char === '[' ||
            char === ']' ||
            char === ':' ||
            char === ',' ||
            char === '.' ||
            char === '=' ||
            char === '+' ||
            char === '-' ||
            char === '*' ||
            char === '/'
          ) {
            if (current) {
              tokens.push(renderPythonToken(current, keywords, builtins))
              current = ''
            }
            tokens.push(char)
          } else {
            current += char
          }
        }

        if (current) {
          if (inString) {
            tokens.push(
              <span className="text-green-600 dark:text-green-400">
                {current}
              </span>
            )
          } else {
            tokens.push(renderPythonToken(current, keywords, builtins))
          }
        }

        return (
          <div key={lineIdx} className="flex">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            <div className="flex-1">{tokens}</div>
          </div>
        )
      })
    }

    if (lang === 'json') {
      return lines.map((line, lineIdx) => {
        // Process JSON with simple token replacement
        let processedLine = line

        // Keys (property names with quotes followed by colon)
        processedLine = processedLine.replace(
          /"([^"]+)"(\s*):/g,
          '<span class="text-purple-600 dark:text-purple-400">"$1"</span>$2:'
        )

        // String values (quotes not followed by colon)
        processedLine = processedLine.replace(
          /:(\s*)"([^"]*)"/g,
          ':<span class="text-green-600 dark:text-green-400">$1"$2"</span>'
        )

        // Booleans
        processedLine = processedLine.replace(
          /\b(true|false)\b/g,
          '<span class="text-orange-600 dark:text-orange-400">$1</span>'
        )

        // Null
        processedLine = processedLine.replace(
          /\bnull\b/g,
          '<span class="text-red-600 dark:text-red-400">null</span>'
        )

        // Numbers
        processedLine = processedLine.replace(
          /:\s*(-?\d+(\.\d+)?)/g,
          ': <span class="text-cyan-600 dark:text-cyan-400">$1</span>'
        )

        return (
          <div key={lineIdx} className="flex">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block shrink-0">
                {lineIdx + 1}
              </span>
            )}
            <div
              className="flex-1 min-w-0 break-all"
              dangerouslySetInnerHTML={{ __html: processedLine }}
            />
          </div>
        )
      })
    }

    if (lang === 'typescript' || lang === 'javascript') {
      const keywords = [
        'import',
        'from',
        'export',
        'const',
        'let',
        'var',
        'function',
        'return',
        'if',
        'else',
        'for',
        'while',
        'class',
        'extends',
        'implements',
        'interface',
        'type',
        'enum',
        'async',
        'await',
        'new',
        'this',
        'super',
        'static',
        'public',
        'private',
        'protected',
        'readonly',
        'default',
      ]
      const types = [
        'string',
        'number',
        'boolean',
        'void',
        'null',
        'undefined',
        'any',
        'unknown',
        'never',
        'React',
        'ReactNode',
        'AppProps',
        'Metadata',
        'NextApiRequest',
        'NextApiResponse',
        'Readonly',
      ]

      return lines.map((line, lineIdx) => {
        // Comments
        if (line.trim().startsWith('//')) {
          return (
            <div key={lineIdx} className="flex">
              {showLineNumbers && (
                <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                  {lineIdx + 1}
                </span>
              )}
              <div className="flex-1 text-muted-foreground opacity-70 italic">
                {line}
              </div>
            </div>
          )
        }

        // Process each line with a simple tokenizer
        const tokens: React.ReactNode[] = []
        let current = ''
        let inString = false
        let stringChar = ''

        for (let i = 0; i < line.length; i++) {
          const char = line[i]

          // Handle strings
          if ((char === '"' || char === "'" || char === '`') && !inString) {
            if (current) {
              tokens.push(renderToken(current, keywords, types))
              current = ''
            }
            inString = true
            stringChar = char
            current = char
          } else if (char === stringChar && inString) {
            current += char
            tokens.push(
              <span className="text-green-600 dark:text-green-400">
                {current}
              </span>
            )
            current = ''
            inString = false
            stringChar = ''
          } else if (inString) {
            current += char
          } else if (
            char === ' ' ||
            char === '(' ||
            char === ')' ||
            char === '{' ||
            char === '}' ||
            char === '[' ||
            char === ']' ||
            char === ':' ||
            char === ';' ||
            char === ',' ||
            char === '.' ||
            char === '<' ||
            char === '>' ||
            char === '=' ||
            char === '!'
          ) {
            if (current) {
              tokens.push(renderToken(current, keywords, types))
              current = ''
            }
            tokens.push(char)
          } else {
            current += char
          }
        }

        if (current) {
          if (inString) {
            tokens.push(
              <span className="text-green-600 dark:text-green-400">
                {current}
              </span>
            )
          } else {
            tokens.push(renderToken(current, keywords, types))
          }
        }

        return (
          <div key={lineIdx} className="flex">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            <div className="flex-1">{tokens}</div>
          </div>
        )
      })
    }

    // Default - no highlighting
    return lines.map((line, i) => (
      <div key={i} className="flex">
        {showLineNumbers && (
          <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
            {i + 1}
          </span>
        )}
        <div className="flex-1">{line || '\u00A0'}</div>
      </div>
    ))
  }

  const renderPythonToken = (
    token: string,
    keywords: string[],
    builtins: string[]
  ) => {
    if (keywords.includes(token)) {
      return (
        <span className="text-purple-600 dark:text-purple-400 font-semibold">
          {token}
        </span>
      )
    }
    if (builtins.includes(token)) {
      return <span className="text-cyan-600 dark:text-cyan-400">{token}</span>
    }
    if (/^\d+(\.\d+)?$/.test(token)) {
      return (
        <span className="text-orange-600 dark:text-orange-400">{token}</span>
      )
    }
    return token
  }

  const renderToken = (token: string, keywords: string[], types: string[]) => {
    if (keywords.includes(token)) {
      return (
        <span className="text-purple-600 dark:text-purple-400 font-semibold">
          {token}
        </span>
      )
    }
    if (types.includes(token)) {
      return <span className="text-cyan-600 dark:text-cyan-400">{token}</span>
    }
    if (/^\d+$/.test(token)) {
      return (
        <span className="text-orange-600 dark:text-orange-400">{token}</span>
      )
    }
    if (token.startsWith('@')) {
      return <span className="text-pink-600 dark:text-pink-400">{token}</span>
    }
    // Check if it might be a component (starts with uppercase)
    if (/^[A-Z]/.test(token)) {
      return <span className="text-blue-600 dark:text-blue-400">{token}</span>
    }
    return token
  }

  return (
    <div className={cn('relative group', className)}>
      {title && (
        <div className="px-4 py-2 bg-muted/50 dark:bg-zinc-900/50 border-b border-border text-xs text-muted-foreground font-mono rounded-t-lg">
          {title}
        </div>
      )}
      <div
        className={cn(
          'relative rounded-lg overflow-hidden',
          'bg-muted/30 dark:bg-zinc-950/50',
          'border border-border',
          'transition-colors duration-200',
          'group-hover:bg-muted/40 dark:group-hover:bg-zinc-950/70',
          title && 'rounded-t-none border-t-0'
        )}
      >
        <pre
          className={cn(
            'p-4 text-sm font-mono',
            wrapLines
              ? 'overflow-x-hidden whitespace-pre-wrap break-all overflow-wrap-anywhere'
              : 'overflow-x-auto'
          )}
        >
          <code
            className={cn(
              `language-${language}`,
              'text-foreground dark:text-zinc-100'
            )}
          >
            {renderHighlightedCode(code, language)}
          </code>
        </pre>
        <div className="absolute top-2 right-2 flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity duration-200">
          <Button
            size="sm"
            variant="ghost"
            className={cn(
              'h-7 px-2',
              'bg-background/80 dark:bg-zinc-800/50',
              'hover:bg-background dark:hover:bg-zinc-800',
              'text-muted-foreground hover:text-foreground',
              'backdrop-blur-sm'
            )}
            onClick={() => setShowLineNumbers(!showLineNumbers)}
            title={showLineNumbers ? 'Hide line numbers' : 'Show line numbers'}
          >
            <Hash
              className={cn(
                'h-3 w-3 mr-1',
                showLineNumbers && 'text-blue-500'
              )}
            />
            <span className="text-xs">Lines</span>
          </Button>
          {!disableWrapToggle && (
            <Button
              size="sm"
              variant="ghost"
              className={cn(
                'h-7 px-2',
                'bg-background/80 dark:bg-zinc-800/50',
                'hover:bg-background dark:hover:bg-zinc-800',
                'text-muted-foreground hover:text-foreground',
                'backdrop-blur-sm'
              )}
              onClick={() => setWrapLines(!wrapLines)}
              title={wrapLines ? 'Disable line wrap' : 'Enable line wrap'}
            >
              <WrapText
                className={cn('h-3 w-3 mr-1', wrapLines && 'text-blue-500')}
              />
              <span className="text-xs">Wrap</span>
            </Button>
          )}
          {showCopy && (
            <Button
              size="sm"
              variant="ghost"
              className={cn(
                'h-7 px-2',
                'bg-background/80 dark:bg-zinc-800/50',
                'hover:bg-background dark:hover:bg-zinc-800',
                'text-muted-foreground hover:text-foreground',
                'backdrop-blur-sm'
              )}
              onClick={handleCopy}
            >
              {copied ? (
                <>
                  <Check className="h-3 w-3 mr-1" />
                  <span className="text-xs">Copied</span>
                </>
              ) : (
                <>
                  <Copy className="h-3 w-3 mr-1" />
                  <span className="text-xs">Copy</span>
                </>
              )}
            </Button>
          )}
        </div>
      </div>
    </div>
  )
}

// Export a variant for inline code
export function InlineCode({
  children,
  className,
}: {
  children: React.ReactNode
  className?: string
}) {
  return (
    <code
      className={cn(
        'px-1.5 py-0.5 rounded bg-muted font-mono text-sm',
        className
      )}
    >
      {children}
    </code>
  )
}
