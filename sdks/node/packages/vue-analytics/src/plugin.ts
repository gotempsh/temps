// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

import type { App, InjectionKey, Plugin } from "vue";
import { createAnalytics, type AnalyticsApi, type AnalyticsOptions } from "@temps-sdk/analytics-core";

export const TempsAnalyticsKey: InjectionKey<AnalyticsApi> = Symbol("TempsAnalytics");

export interface TempsAnalyticsPluginOptions extends AnalyticsOptions {
  /**
   * Provide a pre-built instance instead of constructing one from options.
   * Useful for testing or shared instances across micro-frontends.
   */
  instance?: AnalyticsApi;
}

/**
 * Vue 3 plugin.
 *
 * ```ts
 * import { createApp } from "vue";
 * import { TempsAnalyticsPlugin } from "@temps-sdk/vue-analytics";
 * const app = createApp(App);
 * app.use(TempsAnalyticsPlugin, { basePath: "/api/_temps" });
 * ```
 */
export const TempsAnalyticsPlugin: Plugin<TempsAnalyticsPluginOptions | undefined> = {
  install(app: App, options?: TempsAnalyticsPluginOptions): void {
    const analytics = options?.instance ?? createAnalytics(options);
    app.provide(TempsAnalyticsKey, analytics);
    app.config.globalProperties.$temps = analytics;
    const onUnmount = app.unmount.bind(app);
    app.unmount = (): void => {
      try {
        analytics.destroy();
      } finally {
        onUnmount();
      }
    };
  },
};

declare module "vue" {
  interface ComponentCustomProperties {
    $temps: AnalyticsApi;
  }
}
