// js/telemetry.js

import * as Sentry from "@sentry/browser";
import { BrowserTracing } from "@sentry/tracing";

const DEFAULT_TRACE_TARGETS = [
  /^\/api\//,
  /^\/chunk\//,
  /^\/list/,
  /^\/mine/,
  /^\/report/,
  /^\/simple/,
  /^\/auth/,
  /^\/checkhash/,
];

let telemetryInitialized = false;
let telemetryAttempted = false;

function normalizeTraceTargets(raw) {
  if (!Array.isArray(raw) || raw.length === 0) {
    return [...DEFAULT_TRACE_TARGETS];
  }
  const targets = [];
  for (const entry of raw) {
    if (entry instanceof RegExp) {
      targets.push(entry);
      continue;
    }
    if (typeof entry === "string" && entry.trim()) {
      try {
        targets.push(new RegExp(entry));
        continue;
      } catch {
        // fall through to push string literal
      }
      targets.push(entry);
    }
  }
  return targets.length ? targets : [...DEFAULT_TRACE_TARGETS];
}

export function initTelemetry(config) {
  if (telemetryAttempted) {
    return telemetryInitialized;
  }
  telemetryAttempted = true;
  const sentryConfig = config && config.sentry ? config.sentry : null;
  if (!sentryConfig || !sentryConfig.enabled || !sentryConfig.dsn) {
    return false;
  }

  const sampleRateRaw = sentryConfig.traces_sample_rate;
  const sampleRate = Number.isFinite(sampleRateRaw)
    ? Math.max(0, Math.min(1, sampleRateRaw))
    : 0;
  const traceTargets = normalizeTraceTargets(
    sentryConfig.trace_propagation_targets
  );

  const integrations = [];
  if (typeof Sentry.browserTracingIntegration === "function") {
    integrations.push(
      Sentry.browserTracingIntegration({
        tracePropagationTargets: traceTargets,
      })
    );
  } else {
    integrations.push(
      new BrowserTracing({
        tracePropagationTargets: traceTargets,
      })
    );
  }

  try {
    Sentry.init({
      dsn: sentryConfig.dsn,
      release: sentryConfig.release,
      environment: sentryConfig.environment,
      integrations,
      tracesSampleRate: sampleRate,
      autoSessionTracking: true,
      sendDefaultPii: false,
    });

    Sentry.configureScope((scope) => {
      scope.setTag("service", "juicebox-frontend");
      scope.setTag("runtime", "browser");
      scope.setExtra(
        "trace_propagation_targets",
        traceTargets.map((target) =>
          target instanceof RegExp ? target.toString() : String(target)
        )
      );
    });
    telemetryInitialized = true;
  } catch (err) {
    telemetryInitialized = false;
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] Sentry init failed", err);
    }
  }

  return telemetryInitialized;
}

export function captureException(error, context) {
  if (!telemetryInitialized) return;
  try {
    Sentry.captureException(error, (scope) => {
      if (context && typeof context === "object") {
        scope.setContext("context", context);
      }
    });
  } catch (err) {
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] captureException failed", err);
    }
  }
}

export function isTelemetryEnabled() {
  return telemetryInitialized;
}

export function getTelemetryClient() {
  return telemetryInitialized ? Sentry : null;
}
