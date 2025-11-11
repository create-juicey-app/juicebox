// js/telemetry.js

import * as Sentry from "@sentry/browser";
import { BrowserTracing } from "@sentry/tracing";

const DEFAULT_TRACE_TARGETS = [
  /^\/api\//,
  /^\/chunk\//,
  /^\/list/,
  /^\/mine/,
  /^\/report/,
  /^\/auth/,
  /^\/checkhash/,
  /^\/simple/,
  /^\/upload/,
  /^\/d\//,
  /^\/f\//,
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
  const profileRateRaw = sentryConfig.profiles_sample_rate;
  const normalizedProfileRate = Number.isFinite(profileRateRaw)
    ? Math.max(0, Math.min(1, profileRateRaw))
    : Math.max(0, Math.min(1, sampleRate));
  const effectiveProfileRate =
    sampleRate > 0
      ? Math.min(normalizedProfileRate, sampleRate)
      : normalizedProfileRate;
  const traceTargets = normalizeTraceTargets(
    sentryConfig.trace_propagation_targets
  );

  const integrations = [];
  if (typeof Sentry.browserTracingIntegration === "function") {
    integrations.push(
      Sentry.browserTracingIntegration({
        tracePropagationTargets: traceTargets,
        enableInp: true,
      })
    );
  } else {
    integrations.push(
      new BrowserTracing({
        tracePropagationTargets: traceTargets,
      })
    );
  }

  const profilingIntegrationFactory =
    typeof Sentry.browserProfilingIntegration === "function"
      ? Sentry.browserProfilingIntegration
      : typeof Sentry.profilingIntegration === "function"
      ? Sentry.profilingIntegration
      : null;
  if (profilingIntegrationFactory && effectiveProfileRate > 0) {
    try {
      integrations.push(profilingIntegrationFactory({}));
    } catch (err) {
      if (window?.DEBUG_LOGS) {
        console.warn("[telemetry] profiling integration failed", err);
      }
    }
  }

  try {
    Sentry.init({
      dsn: sentryConfig.dsn,
      release: sentryConfig.release,
      environment: sentryConfig.environment,
      integrations,
      tracesSampleRate: sampleRate,
      profilesSampleRate: effectiveProfileRate,
      autoSessionTracking: true,
      sendDefaultPii: false,
      // Enable distributed tracing
      enableTracing: true,
      // Use a sampler for more granular control
      tracesSampler: (samplingContext) => {
        const { name, attributes } = samplingContext;

        // Always sample critical user flows
        if (name?.includes("upload") || name?.includes("chunk")) {
          return 1.0;
        }

        // Sample auth and ownership operations at higher rate
        if (name?.includes("auth") || name?.includes("mine")) {
          return 0.5;
        }

        // Sample file operations
        if (name?.includes("delete") || name?.includes("download")) {
          return 0.3;
        }

        // Use default rate for everything else
        return sampleRate;
      },
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
      scope.setExtra("profiles_sample_rate", effectiveProfileRate);
      scope.setTag(
        "profiling",
        effectiveProfileRate > 0 ? "enabled" : "disabled"
      );
    });
    telemetryInitialized = true;
    forwardConsoleToSentry();
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

/**
 * Start a new traced span for custom instrumentation
 * @param {string} name - Span name
 * @param {object} options - Span options (op, attributes, etc.)
 * @param {Function} callback - Function to execute within the span
 * @returns {Promise<any>} Result of callback
 */
export async function startSpan(name, options, callback) {
  if (!telemetryInitialized || typeof Sentry.startSpan !== "function") {
    return callback();
  }

  try {
    return await Sentry.startSpan(
      {
        name,
        op: options?.op || "custom",
        attributes: options?.attributes || {},
      },
      callback
    );
  } catch (err) {
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] startSpan failed", err);
    }
    return callback();
  }
}

/**
 * Start an inactive span for manual control
 * @param {string} name - Span name
 * @param {object} options - Span options
 * @returns {object|null} Span object or null
 */
export function startInactiveSpan(name, options = {}) {
  if (!telemetryInitialized || typeof Sentry.startInactiveSpan !== "function") {
    return null;
  }

  try {
    return Sentry.startInactiveSpan({
      name,
      op: options.op || "custom",
      attributes: options.attributes || {},
    });
  } catch (err) {
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] startInactiveSpan failed", err);
    }
    return null;
  }
}

/**
 * Get the currently active span
 * @returns {object|null} Active span or null
 */
export function getActiveSpan() {
  if (!telemetryInitialized || typeof Sentry.getActiveSpan !== "function") {
    return null;
  }

  try {
    return Sentry.getActiveSpan();
  } catch (err) {
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] getActiveSpan failed", err);
    }
    return null;
  }
}

/**
 * Set attributes on a span
 * @param {object} span - Span object
 * @param {object} attributes - Attributes to set
 */
export function setSpanAttributes(span, attributes) {
  if (!span || !attributes) return;

  try {
    if (typeof span.setAttributes === "function") {
      span.setAttributes(attributes);
    } else if (typeof span.setAttribute === "function") {
      Object.entries(attributes).forEach(([key, value]) => {
        span.setAttribute(key, value);
      });
    }
  } catch (err) {
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] setSpanAttributes failed", err);
    }
  }
}

/**
 * End a span with optional status
 * @param {object} span - Span to end
 * @param {object} options - Options (status, etc.)
 */
export function endSpan(span, options = {}) {
  if (!span || typeof span.end !== "function") return;

  try {
    if (options.status && typeof span.setStatus === "function") {
      span.setStatus(options.status);
    }
    span.end();
  } catch (err) {
    if (window?.DEBUG_LOGS) {
      console.warn("[telemetry] endSpan failed", err);
    }
  }
}

const CONSOLE_LEVEL_MAP = {
  log: "info",
  info: "info",
  warn: "warning",
  error: "error",
  debug: "debug",
  trace: "debug",
};

let consoleForwarded = false;

function formatConsoleArg(value) {
  if (typeof value === "string") return value;
  if (value instanceof Error) {
    return `${value.name}: ${value.message}`;
  }
  try {
    return JSON.stringify(value);
  } catch {
    return String(value);
  }
}

function forwardConsoleToSentry() {
  if (consoleForwarded || !telemetryInitialized) return;
  if (typeof window === "undefined" || !window.console) return;
  const target = window.console;
  const originals = {};
  Object.keys(CONSOLE_LEVEL_MAP).forEach((method) => {
    if (typeof target[method] === "function") {
      originals[method] = target[method].bind(target);
    }
  });
  const fallbackError = originals.error || (() => {});
  Object.entries(CONSOLE_LEVEL_MAP).forEach(([method, level]) => {
    if (!originals[method]) return;
    target[method] = (...args) => {
      originals[method](...args);
      if (typeof Sentry.captureEvent !== "function") return;
      try {
        const message = formatConsoleArg(args[0] ?? `[console.${method}]`);
        Sentry.captureEvent({
          message: message,
          level,
          logger: "browser.console",
          extra: {
            console_method: method,
            arguments: args.map(formatConsoleArg),
          },
        });
      } catch (err) {
        fallbackError("[telemetry] console forwarding failed", err);
      }
    };
  });
  consoleForwarded = true;
}
