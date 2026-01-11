import { colors, icons } from '../ui/output.js'

export class CliError extends Error {
  constructor(
    message: string,
    public code: string = 'CLI_ERROR',
    public exitCode: number = 1
  ) {
    super(message)
    this.name = 'CliError'
  }
}

export class AuthenticationError extends CliError {
  constructor(message: string = 'Authentication required') {
    super(message, 'AUTH_ERROR', 1)
    this.name = 'AuthenticationError'
  }
}

export class ApiError extends CliError {
  constructor(
    message: string,
    public statusCode?: number,
    public details?: unknown
  ) {
    super(message, 'API_ERROR', 1)
    this.name = 'ApiError'
  }
}

export class ConfigError extends CliError {
  constructor(message: string) {
    super(message, 'CONFIG_ERROR', 1)
    this.name = 'ConfigError'
  }
}

export class ValidationError extends CliError {
  constructor(message: string) {
    super(message, 'VALIDATION_ERROR', 1)
    this.name = 'ValidationError'
  }
}

export function handleError(error: unknown): never {
  if (error instanceof CliError) {
    console.error(`\n${icons.error} ${colors.error(error.message)}`)
    if (error instanceof ApiError && error.details) {
      console.error(colors.muted(JSON.stringify(error.details, null, 2)))
    }
    process.exit(error.exitCode)
  }

  if (error instanceof Error) {
    console.error(`\n${icons.error} ${colors.error(error.message)}`)
    if (process.env.DEBUG) {
      console.error(colors.muted(error.stack ?? ''))
    }
    process.exit(1)
  }

  console.error(`\n${icons.error} ${colors.error('An unexpected error occurred')}`)
  console.error(colors.muted(String(error)))
  process.exit(1)
}

export function assertNever(value: never): never {
  throw new CliError(`Unexpected value: ${value}`)
}
