import {
  input,
  password,
  confirm,
  select,
  checkbox,
  editor,
} from '@inquirer/prompts'
import search from '@inquirer/search'
import { colors, icons } from './output.js'

export interface TextPromptOptions {
  message: string
  default?: string
  required?: boolean
  validate?: (value: string) => boolean | string | Promise<boolean | string>
  transformer?: (value: string) => string
}

export interface PasswordPromptOptions {
  message: string
  mask?: string
  validate?: (value: string) => boolean | string | Promise<boolean | string>
}

export interface ConfirmPromptOptions {
  message: string
  default?: boolean
}

export interface SelectOption<T = string> {
  name: string
  value: T
  description?: string
  disabled?: boolean | string
}

export interface SelectPromptOptions<T = string> {
  message: string
  choices: SelectOption<T>[]
  default?: T
}

export interface CheckboxPromptOptions<T = string> {
  message: string
  choices: SelectOption<T>[]
  required?: boolean
  validate?: (values: T[]) => boolean | string | Promise<boolean | string>
}

export async function promptText(options: TextPromptOptions): Promise<string> {
  const result = await input({
    message: options.message,
    default: options.default,
    required: options.required ?? false,
    validate: options.validate,
    transformer: options.transformer,
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result
}

export async function promptPassword(options: PasswordPromptOptions): Promise<string> {
  const result = await password({
    message: options.message,
    mask: options.mask ?? '*',
    validate: options.validate,
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result
}

export async function promptConfirm(options: ConfirmPromptOptions): Promise<boolean> {
  const result = await confirm({
    message: options.message,
    default: options.default ?? false,
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result
}

export async function promptSelect<T = string>(options: SelectPromptOptions<T>): Promise<T> {
  const result = await select({
    message: options.message,
    choices: options.choices,
    default: options.default,
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result
}

export async function promptCheckbox<T = string>(options: CheckboxPromptOptions<T>): Promise<T[]> {
  const result = await checkbox({
    message: options.message,
    choices: options.choices,
    required: options.required,
    // Note: validate is not passed as inquirer expects different signature
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result
}

export async function promptEditor(message: string, defaultValue?: string): Promise<string> {
  const result = await editor({
    message,
    default: defaultValue,
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result
}

/**
 * URL prompt with validation
 */
export async function promptUrl(message: string, defaultValue?: string): Promise<string> {
  return promptText({
    message,
    default: defaultValue,
    validate: (value) => {
      try {
        new URL(value)
        return true
      } catch {
        return 'Please enter a valid URL'
      }
    },
  })
}

/**
 * Email prompt with validation
 */
export async function promptEmail(message: string, defaultValue?: string): Promise<string> {
  return promptText({
    message,
    default: defaultValue,
    validate: (value) => {
      const emailRegex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/
      return emailRegex.test(value) || 'Please enter a valid email address'
    },
  })
}

/**
 * Number prompt with optional min/max
 */
export async function promptNumber(
  message: string,
  options?: { default?: number; min?: number; max?: number }
): Promise<number> {
  const result = await promptText({
    message,
    default: options?.default?.toString(),
    validate: (value) => {
      const num = parseInt(value, 10)
      if (isNaN(num)) return 'Please enter a valid number'
      if (options?.min !== undefined && num < options.min) {
        return `Number must be at least ${options.min}`
      }
      if (options?.max !== undefined && num > options.max) {
        return `Number must be at most ${options.max}`
      }
      return true
    },
  })
  return parseInt(result, 10)
}

/**
 * Multi-step wizard helper
 */
export interface WizardStep<T> {
  name: string
  prompt: () => Promise<T>
}

export async function wizard<T extends Record<string, unknown>>(
  title: string,
  steps: WizardStep<T[keyof T]>[]
): Promise<T> {
  console.log()
  console.log(colors.bold(`${icons.sparkles} ${title}`))
  console.log(colors.muted('─'.repeat(40)))
  console.log()

  const result: Record<string, unknown> = {}

  for (const step of steps) {
    result[step.name] = await step.prompt()
  }

  return result as T
}

export interface SearchOption<T = string> {
  name: string
  value: T
  description?: string
}

export interface SearchPromptOptions<T = string> {
  message: string
  choices: SearchOption<T>[]
  pageSize?: number
}

/**
 * Interactive search prompt with fuzzy filtering
 */
export async function promptSearch<T = string>(options: SearchPromptOptions<T>): Promise<T> {
  const result = await search({
    message: options.message,
    pageSize: options.pageSize ?? 10,
    source: async (term) => {
      if (!term) {
        // Show first items when no search term
        return options.choices.slice(0, options.pageSize ?? 10)
      }

      const searchTerm = term.toLowerCase()
      return options.choices.filter(
        (choice) =>
          choice.name.toLowerCase().includes(searchTerm) ||
          (choice.description?.toLowerCase() || '').includes(searchTerm)
      )
    },
    theme: {
      prefix: colors.primary('?'),
    },
  })
  return result as T
}
