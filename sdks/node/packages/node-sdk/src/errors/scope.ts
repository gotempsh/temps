import type { Breadcrumb, Event, Scope as IScope, User } from './types.js';

export class Scope implements IScope {
  private user: User | null = null;
  private tags: Record<string, string> = {};
  private extra: Record<string, any> = {};
  private contexts: Record<string, any> = {};
  private level: Event['level'] | undefined;
  private breadcrumbs: Breadcrumb[] = [];
  private maxBreadcrumbs: number = 100;

  constructor(maxBreadcrumbs: number = 100) {
    this.maxBreadcrumbs = maxBreadcrumbs;
  }

  setUser(user: User | null): void {
    this.user = user;
  }

  setTag(key: string, value: string): void {
    this.tags[key] = value;
  }

  setTags(tags: Record<string, string>): void {
    this.tags = { ...this.tags, ...tags };
  }

  setExtra(key: string, value: any): void {
    this.extra[key] = value;
  }

  setExtras(extras: Record<string, any>): void {
    this.extra = { ...this.extra, ...extras };
  }

  setContext(key: string, context: Record<string, any> | null): void {
    if (context === null) {
      delete this.contexts[key];
    } else {
      this.contexts[key] = context;
    }
  }

  setLevel(level: Event['level']): void {
    this.level = level;
  }

  addBreadcrumb(breadcrumb: Breadcrumb): void {
    const mergedBreadcrumb: Breadcrumb = {
      timestamp: Date.now(),
      ...breadcrumb,
    };

    this.breadcrumbs.push(mergedBreadcrumb);

    if (this.breadcrumbs.length > this.maxBreadcrumbs) {
      this.breadcrumbs.shift();
    }
  }

  clearBreadcrumbs(): void {
    this.breadcrumbs = [];
  }

  clear(): void {
    this.user = null;
    this.tags = {};
    this.extra = {};
    this.contexts = {};
    this.level = undefined;
    this.breadcrumbs = [];
  }

  applyToEvent(event: Event): Event {
    if (this.user) {
      event.user = { ...this.user, ...event.user };
    }

    event.tags = { ...(event.tags || {}), ...this.tags };
    event.extra = { ...(event.extra || {}), ...this.extra };
    event.contexts = { ...(event.contexts || {}), ...this.contexts };

    if (this.level && !event.level) {
      event.level = this.level;
    }

    event.breadcrumbs = [...this.breadcrumbs, ...(event.breadcrumbs || [])];

    return event;
  }

  clone(): Scope {
    const newScope = new Scope(this.maxBreadcrumbs);
    newScope.user = this.user ? { ...this.user } : null;
    newScope.tags = { ...this.tags };
    newScope.extra = { ...this.extra };
    newScope.contexts = { ...this.contexts };
    newScope.level = this.level;
    newScope.breadcrumbs = [...this.breadcrumbs];
    return newScope;
  }
}
