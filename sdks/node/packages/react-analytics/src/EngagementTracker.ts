"use client";
import { sendAnalytics, sendAnalyticsReliable } from "./utils";
import type { JsonValue } from "./types";

export interface EngagementTrackerOptions {
  basePath?: string;
  domain?: string;
  heartbeatInterval?: number;
  inactivityTimeout?: number;
  engagementThreshold?: number;
  onHeartbeat?: (data: EngagementData) => void;
  onPageLeave?: (data: EngagementData) => void;
}

export interface EngagementData {
  engagement_time_seconds: number;
  total_time_seconds: number;
  heartbeat_count: number;
  is_engaged: boolean;
  is_visible: boolean;
  time_since_last_activity: number;
}

export class EngagementTracker {
  private startTime: number;
  private engagementTime: number = 0;
  private lastActivityTime: number;
  private lastEngagementStart: number | null = null;
  private heartbeatCount: number = 0;
  private heartbeatInterval: number;
  private inactivityTimeout: number;
  private engagementThreshold: number;
  private isVisible: boolean;
  private isFocused: boolean;
  private isActive: boolean = true;
  private heartbeatTimer: ReturnType<typeof setInterval> | null = null;
  private inactivityTimer: ReturnType<typeof setTimeout> | null = null;
  private hasTrackedLeave: boolean = false;
  private readonly basePath: string;
  private readonly domain: string;
  private readonly onHeartbeat?: (data: EngagementData) => void;
  private readonly onPageLeave?: (data: EngagementData) => void;

  constructor(options: EngagementTrackerOptions = {}) {
    this.basePath = options.basePath || "/api/_temps";
    this.domain = options.domain || window.location.hostname;
    this.heartbeatInterval = options.heartbeatInterval || 30000; // 30 seconds
    this.inactivityTimeout = options.inactivityTimeout || 30000; // 30 seconds
    this.engagementThreshold = options.engagementThreshold || 10000; // 10 seconds
    this.onHeartbeat = options.onHeartbeat;
    this.onPageLeave = options.onPageLeave;

    this.startTime = Date.now();
    this.lastActivityTime = Date.now();
    this.isVisible = document.visibilityState === "visible";
    this.isFocused = document.hasFocus();

    // Don't start engagement tracking until first user interaction
    this.lastEngagementStart = null;
    this.isActive = false;

    this.setupEventListeners();
    this.startHeartbeat();
  }

  private setupEventListeners(): void {
    // Visibility change
    document.addEventListener("visibilitychange", this.handleVisibilityChange);

    // Focus/blur for better multi-monitor support
    window.addEventListener("focus", this.handleFocus);
    window.addEventListener("blur", this.handleBlur);

    // User activity tracking - use more specific events to avoid false positives
    const activityEvents = ["mousedown", "keypress", "scroll", "touchstart", "click"];
    activityEvents.forEach(event => {
      document.addEventListener(event, this.handleUserActivity, { passive: true });
    });

    // Page leave events
    window.addEventListener("pagehide", this.handlePageLeave);
    window.addEventListener("beforeunload", this.handlePageLeave);

    // SPA navigation
    const originalPushState = window.history.pushState;
    window.history.pushState = (...args) => {
      this.handlePageLeave();
      originalPushState.apply(window.history, args);
      this.reset();
    };

    window.addEventListener("popstate", this.handleNavigation);
  }

  private handleVisibilityChange = (): void => {
    const wasVisible = this.isVisible;
    this.isVisible = document.visibilityState === "visible";

    if (!wasVisible && this.isVisible) {
      // Tab became visible - but don't start engagement until user interacts
      this.startHeartbeat();
    } else if (wasVisible && !this.isVisible) {
      // Tab became hidden - pause engagement
      this.pauseEngagement();
    }
  };

  private handleFocus = (): void => {
    this.isFocused = true;
  };

  private handleBlur = (): void => {
    this.isFocused = false;
    // When window loses focus, pause engagement tracking
    this.pauseEngagement();
  };

  private pauseEngagement(): void {
    this.updateEngagementTime();
    this.lastEngagementStart = null;
    if (!this.isVisible) {
      this.stopHeartbeat();
    }
  }

  private handleUserActivity = (): void => {
    // Only track activity if tab is visible AND focused
    if (!this.isVisible || !this.isFocused) return;

    const now = Date.now();

    if (!this.isActive) {
      // Resume from inactive state
      this.isActive = true;
      this.lastEngagementStart = now;
      this.startHeartbeat();
    }

    this.lastActivityTime = now;
    this.resetInactivityTimer();
  };

  private resetInactivityTimer(): void {
    if (this.inactivityTimer) {
      clearTimeout(this.inactivityTimer);
    }

    this.inactivityTimer = setTimeout(() => {
      if (this.isVisible) {
        this.updateEngagementTime();
        this.isActive = false;
        this.lastEngagementStart = null;
      }
    }, this.inactivityTimeout);
  }

  private updateEngagementTime(): void {
    if (this.lastEngagementStart !== null) {
      const currentEngagement = Date.now() - this.lastEngagementStart;
      this.engagementTime += currentEngagement;
    }
  }

  private startHeartbeat(): void {
    if (this.heartbeatTimer) return;

    this.heartbeatTimer = setInterval(() => {
      // Only send heartbeat if tab is visible AND user has been active
      if (this.isVisible && this.isActive) {
        this.sendHeartbeat();
      }
    }, this.heartbeatInterval);
  }

  private stopHeartbeat(): void {
    if (this.heartbeatTimer) {
      clearInterval(this.heartbeatTimer);
      this.heartbeatTimer = null;
    }
  }

  private async sendHeartbeat(): Promise<void> {
    this.updateEngagementTime();
    this.heartbeatCount++;

    const data = this.getEngagementData();

    if (this.onHeartbeat) {
      this.onHeartbeat(data);
    }

    await sendAnalytics("event", {
      event_name: "heartbeat",
      request_path: window.location.pathname,
      request_query: window.location.search,
      domain: this.domain,
      event_data: {
        engagement_time: Math.round(this.engagementTime),
        total_time: Math.round(Date.now() - this.startTime),
        is_engaged: data.is_engaged,
        is_visible: this.isVisible,
        time_since_last_activity: data.time_since_last_activity,
        heartbeat_count: this.heartbeatCount,
      } as Record<string, JsonValue>,
    }, "POST", this.basePath);

    // Reset engagement start for next period
    if (this.isActive && this.isVisible) {
      this.lastEngagementStart = Date.now();
    }
  }

  private handlePageLeave = (): void => {
    if (this.hasTrackedLeave) return;
    this.hasTrackedLeave = true;

    this.updateEngagementTime();
    const data = this.getEngagementData();

    if (this.onPageLeave) {
      this.onPageLeave(data);
    }

    sendAnalyticsReliable("event", {
      event_name: "page_leave",
      request_path: window.location.pathname,
      request_query: window.location.search,
      domain: this.domain,
      event_data: {
        engagement_time_seconds: data.engagement_time_seconds,
        total_time_seconds: data.total_time_seconds,
        heartbeat_count: this.heartbeatCount,
        was_engaged: data.is_engaged,
        url: window.location.href,
        referrer: document.referrer,
      } as Record<string, JsonValue>,
    }, this.basePath);
  };

  private handleNavigation = (): void => {
    this.handlePageLeave();
    this.reset();
  };

  private getEngagementData(): EngagementData {
    const totalTime = Date.now() - this.startTime;
    const timeSinceLastActivity = Date.now() - this.lastActivityTime;

    return {
      engagement_time_seconds: Math.round(this.engagementTime / 1000),
      total_time_seconds: Math.round(totalTime / 1000),
      heartbeat_count: this.heartbeatCount,
      is_engaged: this.engagementTime >= this.engagementThreshold,
      is_visible: this.isVisible,
      time_since_last_activity: Math.round(timeSinceLastActivity / 1000),
    };
  }

  private reset(): void {
    this.startTime = Date.now();
    this.engagementTime = 0;
    this.lastActivityTime = Date.now();
    this.heartbeatCount = 0;
    this.hasTrackedLeave = false;
    // Reset to inactive state - wait for user interaction
    this.isActive = false;
    this.lastEngagementStart = null;

    if (this.isVisible) {
      this.startHeartbeat();
    }
  }

  public destroy(): void {
    this.handlePageLeave();
    this.stopHeartbeat();

    if (this.inactivityTimer) {
      clearTimeout(this.inactivityTimer);
    }

    document.removeEventListener("visibilitychange", this.handleVisibilityChange);
    window.removeEventListener("focus", this.handleFocus);
    window.removeEventListener("blur", this.handleBlur);
    window.removeEventListener("pagehide", this.handlePageLeave);
    window.removeEventListener("beforeunload", this.handlePageLeave);
    window.removeEventListener("popstate", this.handleNavigation);

    const activityEvents = ["mousedown", "keypress", "scroll", "touchstart", "click"];
    activityEvents.forEach(event => {
      document.removeEventListener(event, this.handleUserActivity);
    });
  }
}
