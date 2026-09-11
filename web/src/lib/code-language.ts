import type { CodeLanguage } from '@/components/ui/code-block'

const aliases: Record<string, CodeLanguage> = {
  bash: 'bash',
  sh: 'shell',
  shell: 'shell',
  shellscript: 'shell',
  zsh: 'shell',
  yml: 'yaml',
  yaml: 'yaml',
  json: 'json',
  js: 'javascript',
  javascript: 'javascript',
  jsx: 'tsx',
  ts: 'typescript',
  typescript: 'typescript',
  tsx: 'tsx',
  py: 'python',
  python: 'python',
  go: 'go',
  golang: 'go',
  rs: 'rust',
  rust: 'rust',
  rb: 'ruby',
  ruby: 'ruby',
  php: 'php',
  java: 'java',
  sql: 'sql',
  docker: 'dockerfile',
  dockerfile: 'dockerfile',
  css: 'css',
  html: 'html',
  md: 'markdown',
  markdown: 'markdown',
  toml: 'toml',
  xml: 'xml',
  text: 'text',
  txt: 'text',
  plaintext: 'text',
}

/** Only resolve grammars bundled by the shared highlighter. */
export function codeLanguage(hint?: string): CodeLanguage {
  return aliases[(hint ?? '').toLowerCase().replace(/^language-/, '')] ?? 'text'
}

export function codeLanguageForFile(filename?: string): CodeLanguage {
  const name = (filename ?? '').split(/[?#]/)[0].split('/').pop() ?? ''
  if (/^dockerfile(?:\.|$)/i.test(name)) return 'dockerfile'
  if (/^\.env(?:\.|$)/.test(name)) return 'bash'
  return codeLanguage(name.split('.').pop())
}
