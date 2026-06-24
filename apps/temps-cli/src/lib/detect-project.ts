/**
 * Local project detection utilities.
 * Detects framework preset, git remote, and service hints from the current directory.
 */
import { existsSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import { execSync } from 'node:child_process'
import { join, basename } from 'node:path'

// ─── Preset Detection ───────────────────────────────────────────────────────

export interface DetectedPreset {
  /** Preset slug matching the API's preset system (e.g., 'nextjs', 'vite') */
  slug: string
  /** Human-readable label */
  label: string
  /** Confidence: 'high' if a framework-specific config file found, 'low' if generic */
  confidence: 'high' | 'low'
}

interface PresetRule {
  /** Files to check for existence (any match triggers the rule) */
  files: string[]
  slug: string
  label: string
}

/** Ordered by specificity — more specific frameworks first */
const PRESET_RULES: PresetRule[] = [
  // JavaScript/TypeScript frameworks (specific → generic)
  {
    files: ['next.config.js', 'next.config.ts', 'next.config.mjs'],
    slug: 'nextjs',
    label: 'Next.js',
  },
  {
    files: ['nuxt.config.ts', 'nuxt.config.js'],
    slug: 'nuxtjs',
    label: 'Nuxt.js',
  },
  {
    files: ['astro.config.mjs', 'astro.config.ts', 'astro.config.js'],
    slug: 'astro',
    label: 'Astro',
  },
  {
    files: ['svelte.config.js', 'svelte.config.ts'],
    slug: 'sveltekit',
    label: 'SvelteKit',
  },
  {
    files: ['remix.config.js', 'remix.config.ts'],
    slug: 'remix',
    label: 'Remix',
  },
  {
    files: ['angular.json'],
    slug: 'angular',
    label: 'Angular',
  },
  {
    files: ['gatsby-config.js', 'gatsby-config.ts'],
    slug: 'gatsby',
    label: 'Gatsby',
  },
  {
    files: ['vite.config.ts', 'vite.config.js', 'vite.config.mjs'],
    slug: 'vite',
    label: 'Vite',
  },
  // Non-JS frameworks
  {
    files: ['Cargo.toml'],
    slug: 'rust',
    label: 'Rust',
  },
  {
    files: ['go.mod'],
    slug: 'go',
    label: 'Go',
  },
  {
    files: ['Gemfile'],
    slug: 'rails',
    label: 'Ruby on Rails',
  },
  {
    files: ['pom.xml', 'build.gradle', 'build.gradle.kts'],
    slug: 'java',
    label: 'Java',
  },
  // Python — detect specific frameworks via pyproject.toml or requirements.txt
  {
    files: ['requirements.txt', 'pyproject.toml', 'setup.py'],
    slug: 'python',
    label: 'Python',
  },
  // Generic fallbacks
  {
    files: ['Dockerfile'],
    slug: 'dockerfile',
    label: 'Dockerfile',
  },
]

/**
 * Detect the project preset from the current directory by scanning for marker files.
 * Returns null if nothing is detected.
 */
export function detectPreset(dir?: string): DetectedPreset | null {
  const projectDir = dir ?? process.cwd()

  for (const rule of PRESET_RULES) {
    const matched = rule.files.some((file) => existsSync(join(projectDir, file)))
    if (matched) {
      return {
        slug: rule.slug,
        label: rule.label,
        confidence: 'high',
      }
    }
  }

  // Check for package.json as generic Node.js
  if (existsSync(join(projectDir, 'package.json'))) {
    return {
      slug: 'nodejs',
      label: 'Node.js',
      confidence: 'low',
    }
  }

  // Check for index.html as static site
  if (existsSync(join(projectDir, 'index.html'))) {
    return {
      slug: 'static',
      label: 'Static Site',
      confidence: 'low',
    }
  }

  return null
}

/**
 * Try to refine Python preset by reading requirements.txt or pyproject.toml
 * for specific frameworks (FastAPI, Django, Flask).
 */
export async function refinePythonPreset(dir?: string): Promise<DetectedPreset> {
  const projectDir = dir ?? process.cwd()

  // Check requirements.txt
  const reqPath = join(projectDir, 'requirements.txt')
  if (existsSync(reqPath)) {
    try {
      const content = await readFile(reqPath, 'utf-8')
      const lower = content.toLowerCase()
      if (lower.includes('fastapi')) return { slug: 'fastapi', label: 'FastAPI', confidence: 'high' }
      if (lower.includes('django')) return { slug: 'django', label: 'Django', confidence: 'high' }
      if (lower.includes('flask')) return { slug: 'flask', label: 'Flask', confidence: 'high' }
    } catch {
      // ignore read errors
    }
  }

  // Check pyproject.toml
  const pyprojectPath = join(projectDir, 'pyproject.toml')
  if (existsSync(pyprojectPath)) {
    try {
      const content = await readFile(pyprojectPath, 'utf-8')
      const lower = content.toLowerCase()
      if (lower.includes('fastapi')) return { slug: 'fastapi', label: 'FastAPI', confidence: 'high' }
      if (lower.includes('django')) return { slug: 'django', label: 'Django', confidence: 'high' }
      if (lower.includes('flask')) return { slug: 'flask', label: 'Flask', confidence: 'high' }
    } catch {
      // ignore read errors
    }
  }

  return { slug: 'python', label: 'Python', confidence: 'low' }
}

// ─── Static Output Detection ─────────────────────────────────────────────────

/**
 * Common build-output directory names, ordered by how strongly they imply a
 * ready-to-serve static bundle. The first one that exists wins.
 */
const STATIC_DIR_CANDIDATES = ['dist', 'build', 'out', 'public', '_site', 'output'] as const

/**
 * Detect a likely static-output directory to deploy (e.g. `dist`, `build`,
 * `out`). Returns the directory name (relative to `dir`) or null when none of
 * the well-known candidates exist. The caller can fall back to prompting.
 *
 * A bare `index.html` in the project root counts too — that's a static site
 * with no build step, deployable as `./`.
 */
export function detectStaticDir(dir?: string): string | null {
  const projectDir = dir ?? process.cwd()
  for (const candidate of STATIC_DIR_CANDIDATES) {
    if (existsSync(join(projectDir, candidate, 'index.html'))) {
      return candidate
    }
  }
  // A built bundle directory without an index.html is still plausible output.
  for (const candidate of STATIC_DIR_CANDIDATES) {
    if (existsSync(join(projectDir, candidate))) {
      return candidate
    }
  }
  if (existsSync(join(projectDir, 'index.html'))) {
    return '.'
  }
  return null
}

// ─── Git Remote Detection ────────────────────────────────────────────────────

export interface DetectedGitRemote {
  /** Full remote URL */
  url: string
  /** Repository owner (org or user) */
  owner: string
  /** Repository name (without .git suffix) */
  repo: string
  /** Host (e.g., 'github.com', 'gitlab.com') */
  host: string
}

/**
 * Detect the git remote URL from the current directory.
 * Parses both HTTPS and SSH formats.
 */
export function detectGitRemote(dir?: string): DetectedGitRemote | null {
  try {
    const url = execSync('git remote get-url origin', {
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
      cwd: dir ?? process.cwd(),
    }).trim()

    if (!url) return null

    return parseGitUrl(url)
  } catch {
    return null
  }
}

/**
 * Parse a git URL (HTTPS or SSH) into its components.
 */
export function parseGitUrl(url: string): DetectedGitRemote | null {
  // HTTPS format: https://github.com/owner/repo.git
  const httpsMatch = url.match(/^https?:\/\/([^/]+)\/([^/]+)\/([^/]+?)(?:\.git)?$/)
  if (httpsMatch) {
    return {
      url,
      host: httpsMatch[1]!,
      owner: httpsMatch[2]!,
      repo: httpsMatch[3]!,
    }
  }

  // SSH format: git@github.com:owner/repo.git
  const sshMatch = url.match(/^git@([^:]+):([^/]+)\/([^/]+?)(?:\.git)?$/)
  if (sshMatch) {
    return {
      url,
      host: sshMatch[1]!,
      owner: sshMatch[2]!,
      repo: sshMatch[3]!,
    }
  }

  return null
}

/**
 * Detect the current git branch.
 */
export function detectGitBranch(dir?: string): string | null {
  try {
    return execSync('git rev-parse --abbrev-ref HEAD', {
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
      cwd: dir ?? process.cwd(),
    }).trim() || null
  } catch {
    return null
  }
}

/**
 * Check if the current directory is inside a git repository.
 */
export function isGitRepo(dir?: string): boolean {
  try {
    execSync('git rev-parse --is-inside-work-tree', {
      encoding: 'utf-8',
      stdio: ['pipe', 'pipe', 'pipe'],
      cwd: dir ?? process.cwd(),
    })
    return true
  } catch {
    return false
  }
}

// ─── Git Commit Detection ────────────────────────────────────────────────────

// ─── Service Hints Detection ─────────────────────────────────────────────────

import type { ServiceTypeRoute } from '../api/types.gen.js'

/**
 * Scan .env files and project configuration for hints about what services are needed.
 * Returns a list of suggested service types.
 */
export async function detectServiceHints(dir?: string): Promise<ServiceTypeRoute[]> {
  const projectDir = dir ?? process.cwd()
  const hints = new Set<ServiceTypeRoute>()

  // Check .env files for common environment variables
  const envFiles = ['.env', '.env.example', '.env.local', '.env.development']
  for (const envFile of envFiles) {
    const envPath = join(projectDir, envFile)
    if (existsSync(envPath)) {
      try {
        const content = await readFile(envPath, 'utf-8')
        const upper = content.toUpperCase()

        if (upper.includes('DATABASE_URL') || upper.includes('POSTGRES') || upper.includes('PG_')) {
          hints.add('postgres')
        }
        if (upper.includes('REDIS_URL') || upper.includes('REDIS_HOST')) {
          hints.add('redis')
        }
        if (
          upper.includes('S3_BUCKET') ||
          upper.includes('S3_ENDPOINT') ||
          upper.includes('AWS_S3') ||
          upper.includes('MINIO')
        ) {
          hints.add('s3')
        }
        if (upper.includes('MONGODB_URI') || upper.includes('MONGO_URL')) {
          hints.add('mongodb')
        }
      } catch {
        // ignore read errors
      }
    }
  }

  // Check for Prisma schema
  const prismaPath = join(projectDir, 'prisma', 'schema.prisma')
  if (existsSync(prismaPath)) {
    try {
      const content = await readFile(prismaPath, 'utf-8')
      if (content.includes('postgresql') || content.includes('postgres')) {
        hints.add('postgres')
      }
      if (content.includes('mongodb')) {
        hints.add('mongodb')
      }
    } catch {
      // ignore
    }
  }

  // Check for drizzle config
  const drizzleFiles = ['drizzle.config.ts', 'drizzle.config.js']
  for (const drizzleFile of drizzleFiles) {
    if (existsSync(join(projectDir, drizzleFile))) {
      hints.add('postgres') // Most drizzle projects use postgres
      break
    }
  }

  return Array.from(hints)
}

// ─── Project Name Suggestion ─────────────────────────────────────────────────

/**
 * Suggest a project name based on directory name, git remote, or package.json.
 */
export async function suggestProjectName(dir?: string): Promise<string> {
  const projectDir = dir ?? process.cwd()

  // Try package.json name
  const pkgPath = join(projectDir, 'package.json')
  if (existsSync(pkgPath)) {
    try {
      const content = await readFile(pkgPath, 'utf-8')
      const pkg = JSON.parse(content)
      if (pkg.name && typeof pkg.name === 'string') {
        // Clean scoped package names: @org/name → name
        const name = pkg.name.replace(/^@[^/]+\//, '')
        if (name.length >= 2) {
          return sanitizeProjectName(name)
        }
      }
    } catch {
      // ignore
    }
  }

  // Try git remote repo name
  const remote = detectGitRemote(projectDir)
  if (remote) {
    return sanitizeProjectName(remote.repo)
  }

  // Fall back to directory name
  return sanitizeProjectName(basename(projectDir))
}

/**
 * Sanitize a string into a valid project name (slug-like).
 */
function sanitizeProjectName(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9-]/g, '-')
    .replace(/-+/g, '-')
    .replace(/^-|-$/g, '')
    .slice(0, 60)
}
