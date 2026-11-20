// SPDX-FileCopyrightText: 2024-2026 Temps Contributors
// SPDX-License-Identifier: MIT OR Apache-2.0

// Shared instrumentation for the synthetic local application and SDK runner.
// No input values, DOM text, user identities or arbitrary event names are logged.
window.installSyntheticTelemetry = function installSyntheticTelemetry(S) {
  const actions = new Set([
    "product.plan",
    "product.create",
    "product.ship",
    "pricing.monthly",
    "pricing.yearly",
    "pricing.team_size",
    "faq.opened",
    "signup.opened",
    "signup.closed",
    "signup.validation",
    "signup.template",
    "signup.completed",
    "onboarding.checked",
  ]);
  const attributes = { "service.name": "synthetic-landing" };
  let stopped = false;
  let inFlight = false;
  let samples = 0;
  let timer;
  const abort = new AbortController();

  const onAction = (event) => {
    const action = event.detail;
    if (!actions.has(action)) return;
    S.addBreadcrumb({
      category: "synthetic.ui",
      message: action,
      level: "info",
    });
    S.startSpan(
      { name: "Synthetic landing interaction", op: "ui.event", attributes },
      () => {
        const fields = { ...attributes, "synthetic.action": action };
        if (action === "signup.validation")
          S.logger.warn(
            "Sample signup validation needs a name and valid email",
            fields,
          );
        else S.logger.info("Synthetic landing interaction", fields);
      },
    );
  };

  async function sample() {
    if (stopped || inFlight || samples >= 30) return;
    inFlight = true;
    samples++;
    try {
      await S.startSpan(
        {
          name: "Synthetic landing sample request",
          op: "http.client",
          attributes,
        },
        async () => {
          const started = performance.now();
          const response = await fetch("/synthetic-work.json", {
            cache: "no-store",
            signal: abort.signal,
          });
          if (!response.ok) throw new Error("Synthetic sample request failed");
          await response.json();
          const duration = performance.now() - started;
          const checklist = document.querySelector(".checklist");
          const pending = checklist
            ? checklist.querySelectorAll('button[aria-pressed="false"]').length
            : 3;
          S.metrics.count("synthetic.browser.requests", 1, { attributes });
          S.metrics.gauge("synthetic.browser.queue", pending, { attributes });
          S.metrics.distribution("synthetic.browser.duration", duration, {
            unit: "millisecond",
            attributes,
          });
          S.logger.info("Synthetic landing sample completed", {
            ...attributes,
            "synthetic.duration_ms": duration,
            "synthetic.pending_tasks": pending,
            "http.response.status_code": response.status,
          });
        },
      );
    } catch {
      if (!stopped)
        S.logger.error("Synthetic landing sample request failed", attributes);
    } finally {
      inFlight = false;
      if (samples >= 30) clearInterval(timer);
    }
  }

  function stop() {
    stopped = true;
    clearInterval(timer);
    abort.abort();
    window.removeEventListener("synthetic-action", onAction);
    window.removeEventListener("pagehide", stop);
  }
  window.addEventListener("synthetic-action", onAction);
  window.addEventListener("pagehide", stop);
  // At most one request in flight, six per minute, for at most five minutes.
  timer = setInterval(sample, 10000);
  window.SyntheticTelemetry = { sample, stop };
  void sample();
};
