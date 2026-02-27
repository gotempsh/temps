/**
 * TypeScript types for the external plugin manifest.
 * Must match the Rust types in temps-core::external_plugin::manifest.
 */

/** Where the plugin's nav entry appears in the Temps UI sidebar. */
export type NavSection = 'platform' | 'settings' | 'project'

/** A navigation entry that the plugin contributes to the Temps UI. */
export interface NavEntry {
  /** Display label in the sidebar */
  label: string
  /** Lucide icon name (e.g., "puzzle", "database", "activity") */
  icon: string
  /** Which sidebar section this entry belongs to */
  section: NavSection
  /** Client-side route path (e.g., "/my-plugin") */
  path: string
  /** Sort order within the section (lower = higher in list) */
  order: number
}

/** A client-side route provided by the plugin UI. */
export interface UiRoute {
  /** Route path pattern (e.g., "/my-plugin/:id") */
  path: string
  /** Page title for breadcrumbs */
  title: string
}

/** Describes the plugin's embedded UI bundle. */
export interface UiManifest {
  /** JavaScript entry point filename relative to the bundle root */
  entry_js: string
  /** CSS files to load */
  css: string[]
  /** Client-side routes the plugin handles */
  routes: UiRoute[]
}

/** The complete plugin manifest returned by /api/x/plugins. */
export interface PluginManifest {
  /** Unique plugin identifier (kebab-case, e.g., "backup-manager") */
  name: string
  /** SemVer version string */
  version: string
  /** Human-readable display name */
  display_name?: string | null
  /** Short description of what the plugin does */
  description?: string | null
  /** Navigation entries for the UI sidebar */
  nav: NavEntry[]
  /** UI bundle manifest (if the plugin has a UI) */
  ui?: UiManifest | null
  /** Whether the plugin needs database access */
  requires_db: boolean
  /** Health check endpoint path (relative to plugin root) */
  health_path: string
}
