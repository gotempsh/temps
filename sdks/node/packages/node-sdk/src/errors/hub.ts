import type { Breadcrumb, CaptureContext, CaptureContextScope, Event, User } from './types.js';
import { Scope } from './scope.js';

export class Hub {
  private scopes: Scope[] = [];
  private globalScope: Scope;

  constructor(maxBreadcrumbs: number = 100) {
    this.globalScope = new Scope(maxBreadcrumbs);
    this.scopes.push(this.globalScope);
  }

  pushScope(): Scope {
    const scope = this.getScope().clone();
    this.scopes.push(scope);
    return scope;
  }

  popScope(): void {
    if (this.scopes.length > 1) {
      this.scopes.pop();
    }
  }

  withScope(callback: (scope: Scope) => void): void {
    const scope = this.pushScope();
    try {
      callback(scope);
    } finally {
      this.popScope();
    }
  }

  getScope(): Scope {
    return this.scopes[this.scopes.length - 1];
  }

  configureScope(callback: (scope: Scope) => void): void {
    const scope = this.getScope();
    callback(scope);
  }

  setUser(user: User | null): void {
    this.getScope().setUser(user);
  }

  setTag(key: string, value: string): void {
    this.getScope().setTag(key, value);
  }

  setTags(tags: Record<string, string>): void {
    this.getScope().setTags(tags);
  }

  setExtra(key: string, value: any): void {
    this.getScope().setExtra(key, value);
  }

  setExtras(extras: Record<string, any>): void {
    this.getScope().setExtras(extras);
  }

  setContext(key: string, context: Record<string, any> | null): void {
    this.getScope().setContext(key, context);
  }

  setLevel(level: Event['level']): void {
    this.getScope().setLevel(level);
  }

  addBreadcrumb(breadcrumb: Breadcrumb): void {
    this.getScope().addBreadcrumb(breadcrumb);
  }

  clearBreadcrumbs(): void {
    this.getScope().clearBreadcrumbs();
  }

  applyToEvent(event: Event, captureContext?: CaptureContext): Event {
    let finalEvent = event;

    for (const scope of this.scopes) {
      finalEvent = scope.applyToEvent(finalEvent);
    }

    if (captureContext) {
      if (typeof captureContext === 'function') {
        const tempScope = this.getScope().clone();
        captureContext(tempScope);
        finalEvent = tempScope.applyToEvent(finalEvent);
      } else {
        const tempScope = this.getScope().clone();
        const contextScope = captureContext as CaptureContextScope;

        if (contextScope.user !== undefined) {
          tempScope.setUser(contextScope.user);
        }
        if (contextScope.tags) {
          tempScope.setTags(contextScope.tags);
        }
        if (contextScope.extra) {
          tempScope.setExtras(contextScope.extra);
        }
        if (contextScope.contexts) {
          Object.keys(contextScope.contexts).forEach(key => {
            tempScope.setContext(key, contextScope.contexts![key]);
          });
        }
        if (contextScope.level) {
          tempScope.setLevel(contextScope.level);
        }

        finalEvent = tempScope.applyToEvent(finalEvent);

        // Override level directly if specified in captureContext
        if (contextScope.level) {
          finalEvent.level = contextScope.level;
        }
      }
    }

    return finalEvent;
  }
}
