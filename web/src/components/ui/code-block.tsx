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
    | 'go'
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

  const visibleButtonCount =
    1 + (disableWrapToggle ? 0 : 1) + (showCopy ? 1 : 0)

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
        <span key={i} className="block">
          {showLineNumbers && (
            <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
              {i + 1}
            </span>
          )}
          {line.trim().startsWith('#') ? (
            <span className="text-muted-foreground opacity-70 italic">
              {line || '\u00A0'}
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
        </span>
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
            <span key={lineIdx} className="block">
              {showLineNumbers && (
                <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                  {lineIdx + 1}
                </span>
              )}
              <span className="text-muted-foreground opacity-70 italic">
                {line || '\u00A0'}
              </span>
            </span>
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
          <span key={lineIdx} className="block">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            {tokens.length === 0 ? '\u00A0' : tokens}
          </span>
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
          <span key={lineIdx} className="block">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            <span
              dangerouslySetInnerHTML={{ __html: processedLine || '&nbsp;' }}
            />
          </span>
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
            <span key={lineIdx} className="block">
              {showLineNumbers && (
                <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                  {lineIdx + 1}
                </span>
              )}
              <span className="text-muted-foreground opacity-70 italic">
                {line || '\u00A0'}
              </span>
            </span>
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
          <span key={lineIdx} className="block">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            {tokens.length === 0 ? '\u00A0' : tokens}
          </span>
        )
      })
    }

    if (lang === 'go') {
      const keywords = [
        'package',
        'import',
        'func',
        'return',
        'if',
        'else',
        'for',
        'range',
        'switch',
        'case',
        'default',
        'break',
        'continue',
        'goto',
        'fallthrough',
        'defer',
        'go',
        'chan',
        'select',
        'type',
        'struct',
        'interface',
        'map',
        'var',
        'const',
        'nil',
        'true',
        'false',
      ]
      const types = [
        'string',
        'int',
        'int8',
        'int16',
        'int32',
        'int64',
        'uint',
        'uint8',
        'uint16',
        'uint32',
        'uint64',
        'float32',
        'float64',
        'bool',
        'byte',
        'rune',
        'error',
        'any',
      ]

      return lines.map((line, lineIdx) => {
        // Comments
        if (line.trim().startsWith('//')) {
          return (
            <span key={lineIdx} className="block">
              {showLineNumbers && (
                <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                  {lineIdx + 1}
                </span>
              )}
              <span className="text-muted-foreground opacity-70 italic">
                {line || '\u00A0'}
              </span>
            </span>
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
              tokens.push(renderGoToken(current, keywords, types))
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
            char === '!' ||
            char === '&' ||
            char === '*'
          ) {
            if (current) {
              tokens.push(renderGoToken(current, keywords, types))
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
            tokens.push(renderGoToken(current, keywords, types))
          }
        }

        return (
          <span key={lineIdx} className="block">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            {tokens.length === 0 ? '\u00A0' : tokens}
          </span>
        )
      })
    }

    if (lang === 'yaml') {
      return lines.map((line, lineIdx) => {
        // Comments
        if (line.trim().startsWith('#')) {
          return (
            <span key={lineIdx} className="block">
              {showLineNumbers && (
                <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                  {lineIdx + 1}
                </span>
              )}
              <span className="text-muted-foreground opacity-70 italic">
                {line || '\u00A0'}
              </span>
            </span>
          )
        }

        // Preserve leading whitespace for indentation
        const leadingWs = line.match(/^(\s*)/)?.[1] ?? ''
        const rest = line.slice(leadingWs.length)

        // List items: leading "- "
        let listDash: string | null = null
        let afterList = rest
        if (rest.startsWith('- ')) {
          listDash = '- '
          afterList = rest.slice(2)
        } else if (rest === '-') {
          listDash = '-'
          afterList = ''
        }

        // Match `key:` optionally followed by a value
        const keyMatch = afterList.match(/^([^:#\s][^:]*?):(\s*)(.*)$/)

        const renderValue = (value: string) => {
          const trimmed = value.trim()
          if (trimmed === '') return value
          // Block scalar markers
          if (trimmed === '|' || trimmed === '>' || trimmed === '|-' || trimmed === '>-') {
            return (
              <span className="text-orange-600 dark:text-orange-400">
                {value}
              </span>
            )
          }
          // Quoted strings
          if (
            (trimmed.startsWith('"') && trimmed.endsWith('"')) ||
            (trimmed.startsWith("'") && trimmed.endsWith("'"))
          ) {
            return (
              <span className="text-green-600 dark:text-green-400">{value}</span>
            )
          }
          // Booleans / null
          if (['true', 'false', 'null', '~', 'yes', 'no'].includes(trimmed)) {
            return (
              <span className="text-orange-600 dark:text-orange-400">{value}</span>
            )
          }
          // Numbers
          if (/^-?\d+(\.\d+)?$/.test(trimmed)) {
            return (
              <span className="text-cyan-600 dark:text-cyan-400">{value}</span>
            )
          }
          // Plain string value
          return <span className="text-green-600 dark:text-green-400">{value}</span>
        }

        return (
          <span key={lineIdx} className="block">
            {showLineNumbers && (
              <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
                {lineIdx + 1}
              </span>
            )}
            {leadingWs && <span>{leadingWs}</span>}
            {listDash && (
              <span className="text-muted-foreground">{listDash}</span>
            )}
            {keyMatch ? (
              <>
                <span className="text-purple-600 dark:text-purple-400">
                  {keyMatch[1]}
                </span>
                <span className="text-muted-foreground">:</span>
                {keyMatch[2]}
                {keyMatch[3] && renderValue(keyMatch[3])}
              </>
            ) : (
              afterList && renderValue(afterList)
            )}
            {!leadingWs && !listDash && !keyMatch && !afterList && '\u00A0'}
          </span>
        )
      })
    }

    // Default - no highlighting
    return lines.map((line, i) => (
      <span key={i} className="block">
        {showLineNumbers && (
          <span className="text-muted-foreground/40 select-none pr-4 text-right min-w-[3ch] inline-block">
            {i + 1}
          </span>
        )}
        {line || '\u00A0'}
      </span>
    ))
  }

  const renderGoToken = (token: string, keywords: string[], types: string[]) => {
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
    // Check if it might be a type (starts with uppercase)
    if (/^[A-Z]/.test(token)) {
      return <span className="text-blue-600 dark:text-blue-400">{token}</span>
    }
    return token
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
        <div className="px-4 py-2 bg-zinc-200/70 dark:bg-zinc-900/50 border-b border-border text-xs text-muted-foreground font-mono rounded-t-lg">
          {title}
        </div>
      )}
      <div
        className={cn(
          'relative rounded-lg min-w-0',
          'bg-zinc-100 dark:bg-zinc-950/50',
          'border border-border',
          'transition-colors duration-200',
          'hover:bg-zinc-100/80 dark:group-hover:bg-zinc-950/70',
          title && 'rounded-t-none border-t-0'
        )}
      >
        <div
          className={cn(
            // w-0 min-w-full max-w-full is the trick: forces this element to
            // parent width while letting its children overflow horizontally.
            'w-0 min-w-full max-w-full rounded-lg py-3.5 pl-4',
            visibleButtonCount === 3 && 'pr-28',
            visibleButtonCount === 2 && 'pr-20',
            visibleButtonCount === 1 && 'pr-12',
            visibleButtonCount === 0 && 'pr-4',
            wrapLines ? 'overflow-x-hidden' : 'overflow-x-auto'
          )}
        >
          <pre
            className={cn(
              'text-sm font-mono leading-6 m-0',
              wrapLines
                ? 'whitespace-pre-wrap break-all'
                : 'whitespace-pre'
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
        </div>
        <div className="absolute top-2 right-2 flex items-center gap-0.5 opacity-0 group-hover:opacity-100 focus-within:opacity-100 transition-opacity duration-200">
          <Button
            size="icon"
            variant="ghost"
            className={cn(
              'size-7 rounded-md',
              'text-muted-foreground/70 hover:text-foreground',
              'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
            )}
            onClick={() => setShowLineNumbers(!showLineNumbers)}
            title={showLineNumbers ? 'Hide line numbers' : 'Show line numbers'}
            aria-label={
              showLineNumbers ? 'Hide line numbers' : 'Show line numbers'
            }
          >
            <Hash
              className={cn(
                'size-3.5',
                showLineNumbers && 'text-blue-500 dark:text-blue-400'
              )}
            />
          </Button>
          {!disableWrapToggle && (
            <Button
              size="icon"
              variant="ghost"
              className={cn(
                'size-7 rounded-md',
                'text-muted-foreground/70 hover:text-foreground',
                'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
              )}
              onClick={() => setWrapLines(!wrapLines)}
              title={wrapLines ? 'Disable line wrap' : 'Enable line wrap'}
              aria-label={wrapLines ? 'Disable line wrap' : 'Enable line wrap'}
            >
              <WrapText
                className={cn(
                  'size-3.5',
                  wrapLines && 'text-blue-500 dark:text-blue-400'
                )}
              />
            </Button>
          )}
          {showCopy && (
            <Button
              size="icon"
              variant="ghost"
              className={cn(
                'size-7 rounded-md',
                'text-muted-foreground/70 hover:text-foreground',
                'hover:bg-zinc-200/70 dark:hover:bg-zinc-800/60'
              )}
              onClick={handleCopy}
              title={copied ? 'Copied' : 'Copy'}
              aria-label={copied ? 'Copied' : 'Copy'}
            >
              {copied ? (
                <Check className="size-3.5 text-emerald-600 dark:text-emerald-400" />
              ) : (
                <Copy className="size-3.5" />
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
