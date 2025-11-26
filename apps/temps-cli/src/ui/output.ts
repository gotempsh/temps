import chalk from 'chalk'

export const colors = {
  primary: chalk.cyan,
  success: chalk.green,
  warning: chalk.yellow,
  error: chalk.red,
  info: chalk.blue,
  muted: chalk.gray,
  bold: chalk.bold,
  dim: chalk.dim,
}

export const icons = {
  success: chalk.green('✓'),
  error: chalk.red('✗'),
  warning: chalk.yellow('⚠'),
  info: chalk.blue('ℹ'),
  arrow: chalk.cyan('→'),
  bullet: chalk.gray('•'),
  star: chalk.yellow('★'),
  check: chalk.green('✔'),
  cross: chalk.red('✘'),
  rocket: '🚀',
  package: '📦',
  globe: '🌐',
  key: '🔑',
  lock: '🔒',
  folder: '📁',
  file: '📄',
  clock: '🕐',
  sparkles: '✨',
}

export function success(message: string): void {
  console.log(`${icons.success} ${colors.success(message)}`)
}

export function error(message: string): void {
  console.error(`${icons.error} ${colors.error(message)}`)
}

export function warning(message: string): void {
  console.warn(`${icons.warning} ${colors.warning(message)}`)
}

export function info(message: string): void {
  console.log(`${icons.info} ${colors.info(message)}`)
}

export function log(message: string): void {
  console.log(message)
}

export function newline(): void {
  console.log()
}

export function header(title: string): void {
  newline()
  console.log(colors.bold(title))
  console.log(colors.muted('─'.repeat(Math.min(title.length + 4, 60))))
}

export function keyValue(key: string, value: string | number | boolean | null | undefined): void {
  const displayValue = value === null || value === undefined ? colors.muted('not set') : String(value)
  console.log(`  ${colors.muted(key + ':')} ${displayValue}`)
}

export function list(items: string[], prefix = icons.bullet): void {
  items.forEach((item) => console.log(`  ${prefix} ${item}`))
}

export function box(content: string, title?: string): void {
  const lines = content.split('\n')
  const maxLength = Math.max(...lines.map((l) => l.length), title?.length ?? 0)
  const width = maxLength + 4

  const top = title
    ? `╭─ ${colors.bold(title)} ${'─'.repeat(width - title.length - 4)}╮`
    : `╭${'─'.repeat(width)}╮`

  console.log(colors.muted(top))
  lines.forEach((line) => {
    const padding = ' '.repeat(width - line.length - 2)
    console.log(colors.muted('│') + ` ${line}${padding}` + colors.muted('│'))
  })
  console.log(colors.muted(`╰${'─'.repeat(width)}╯`))
}

export function json(data: unknown): void {
  console.log(JSON.stringify(data, null, 2))
}

export function formatDate(date: string | Date): string {
  const d = typeof date === 'string' ? new Date(date) : date
  return d.toLocaleString()
}

export function formatRelativeTime(date: string | Date): string {
  const d = typeof date === 'string' ? new Date(date) : typeof date === 'number' ? new Date(date) : date
  const now = new Date()
  const diffMs = now.getTime() - d.getTime()
  const diffSecs = Math.floor(diffMs / 1000)
  const diffMins = Math.floor(diffSecs / 60)
  const diffHours = Math.floor(diffMins / 60)
  const diffDays = Math.floor(diffHours / 24)

  if (diffSecs < 60) return 'just now'
  if (diffMins < 60) return `${diffMins}m ago`
  if (diffHours < 24) return `${diffHours}h ago`
  if (diffDays < 7) return `${diffDays}d ago`
  return d.toLocaleDateString()
}

export function truncate(str: string, maxLength: number): string {
  if (str.length <= maxLength) return str
  return str.slice(0, maxLength - 3) + '...'
}
