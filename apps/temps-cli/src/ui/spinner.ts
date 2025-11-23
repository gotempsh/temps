import ora, { type Ora } from 'ora'
import { colors, icons } from './output.js'

export interface SpinnerOptions {
  text: string
  color?: 'cyan' | 'green' | 'yellow' | 'red' | 'blue' | 'magenta' | 'white'
}

let currentSpinner: Ora | null = null

export function startSpinner(options: SpinnerOptions | string): Ora {
  const opts = typeof options === 'string' ? { text: options } : options

  // Stop any existing spinner
  if (currentSpinner) {
    currentSpinner.stop()
  }

  currentSpinner = ora({
    text: opts.text,
    color: opts.color ?? 'cyan',
    spinner: 'dots',
  }).start()

  return currentSpinner
}

export function stopSpinner(): void {
  if (currentSpinner) {
    currentSpinner.stop()
    currentSpinner = null
  }
}

export function succeedSpinner(text?: string): void {
  if (currentSpinner) {
    currentSpinner.succeed(text)
    currentSpinner = null
  }
}

export function failSpinner(text?: string): void {
  if (currentSpinner) {
    currentSpinner.fail(text)
    currentSpinner = null
  }
}

export function warnSpinner(text?: string): void {
  if (currentSpinner) {
    currentSpinner.warn(text)
    currentSpinner = null
  }
}

export function updateSpinner(text: string): void {
  if (currentSpinner) {
    currentSpinner.text = text
  }
}

/**
 * Execute an async function with a spinner
 */
export async function withSpinner<T>(
  text: string,
  fn: () => Promise<T>,
  options?: {
    successText?: string | ((result: T) => string)
    failText?: string | ((error: Error) => string)
  }
): Promise<T> {
  const spinner = startSpinner(text)

  try {
    const result = await fn()
    const successText =
      typeof options?.successText === 'function'
        ? options.successText(result)
        : options?.successText ?? text
    spinner.succeed(successText)
    return result
  } catch (err) {
    const error = err instanceof Error ? err : new Error(String(err))
    const failText =
      typeof options?.failText === 'function'
        ? options.failText(error)
        : options?.failText ?? `${text} - ${colors.error(error.message)}`
    spinner.fail(failText)
    throw error
  }
}

/**
 * Execute multiple async operations with progress indication
 */
export async function withProgress<T>(
  items: T[],
  fn: (item: T, index: number) => Promise<void>,
  options: {
    text: (item: T, index: number, total: number) => string
  }
): Promise<void> {
  const total = items.length
  if (total === 0) return

  const firstItem = items[0] as T
  const spinner = startSpinner(options.text(firstItem, 0, total))

  for (let i = 0; i < items.length; i++) {
    const item = items[i] as T
    spinner.text = options.text(item, i, total)
    await fn(item, i)
  }

  spinner.succeed(`Completed ${total} items`)
}
