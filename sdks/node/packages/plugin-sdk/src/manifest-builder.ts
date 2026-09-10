// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

/**
 * Builder pattern for constructing PluginManifest instances.
 *
 * Mirrors the Rust `PluginManifest::builder()` pattern.
 */

import type { NavEntry, NavSection, PluginManifest, UiManifest } from "./types.js";

export class ManifestBuilder {
  private _name: string;
  private _version: string;
  private _displayName?: string;
  private _description?: string;
  private _nav: NavEntry[] = [];
  private _ui?: UiManifest;
  private _requiresDb = false;
  private _healthPath = "/health";
  private _events: string[] = [];

  constructor(name: string, version: string) {
    this._name = name;
    this._version = version;
  }

  displayName(name: string): this {
    this._displayName = name;
    return this;
  }

  description(desc: string): this {
    this._description = desc;
    return this;
  }

  nav(entry: NavEntry): this {
    this._nav.push(entry);
    return this;
  }

  /**
   * Add a navigation entry using shorthand parameters.
   */
  addNav(
    label: string,
    icon: string,
    path: string,
    options?: { section?: NavSection; order?: number }
  ): this {
    this._nav.push({
      label,
      icon,
      path,
      section: options?.section ?? "platform",
      order: options?.order ?? 50,
    });
    return this;
  }

  ui(manifest: UiManifest): this {
    this._ui = manifest;
    return this;
  }

  requiresDb(requires: boolean): this {
    this._requiresDb = requires;
    return this;
  }

  healthPath(path: string): this {
    this._healthPath = path;
    return this;
  }

  event(eventType: string): this {
    this._events.push(eventType);
    return this;
  }

  events(eventTypes: string[]): this {
    this._events.push(...eventTypes);
    return this;
  }

  build(): PluginManifest {
    return {
      name: this._name,
      version: this._version,
      display_name: this._displayName,
      description: this._description,
      nav: this._nav,
      ui: this._ui,
      requires_db: this._requiresDb,
      health_path: this._healthPath,
      events: this._events,
    };
  }
}

/**
 * Create a new manifest builder.
 *
 * @example
 * ```ts
 * const manifest = createManifest("my-plugin", "0.1.0")
 *   .displayName("My Plugin")
 *   .description("Does amazing things")
 *   .addNav("Dashboard", "layout-dashboard", "/dashboard")
 *   .event("deployment.succeeded")
 *   .build();
 * ```
 */
export function createManifest(
  name: string,
  version: string
): ManifestBuilder {
  return new ManifestBuilder(name, version);
}
