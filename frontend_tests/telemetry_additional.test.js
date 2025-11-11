import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

// Shared mock scope used by Sentry.configureScope
const SCOPE = {
  setTag: jest.fn(),
  setExtra: jest.fn(),
  setContext: jest.fn(),
};

function resetScopeMocks() {
  SCOPE.setTag.mockClear();
  SCOPE.setExtra.mockClear();
  SCOPE.setContext.mockClear();
}

function mockSentryBase(overrides = {}) {
  const base = {
    __esModule: true,
    init: jest.fn(),
    configureScope: (cb) => cb(SCOPE),
    // Leave browserTracingIntegration undefined to force BrowserTracing path when desired
    browserTracingIntegration: undefined,
    // Profiling off by default
    browserProfilingIntegration: undefined,
    profilingIntegration: undefined,
    startSpan: jest.fn(async (_opts, cb) => {
      return await cb();
    }),
    startInactiveSpan: jest.fn((_opts) => ({
      end: jest.fn(),
      setStatus: jest.fn(),
      setAttributes: jest.fn(),
      setAttribute: jest.fn(),
    })),
    getActiveSpan: jest.fn(() => null),
    captureException: jest.fn(),
    captureEvent: jest.fn(),
  };
  return { ...base, ...overrides };
}

function mockTracingBase(overrides = {}) {
  const base = {
    __esModule: true,
    BrowserTracing: function BrowserTracing(opts) {
      this.opts = opts;
    },
  };
  return { ...base, ...overrides };
}

async function importTelemetryWithCustomMocks(
  mockSentryOverrides = {},
  mockTracingOverrides = {},
) {
  jest.resetModules();
  resetScopeMocks();

  jest.mock("@sentry/browser", () => mockSentryBase(mockSentryOverrides));
  jest.mock("@sentry/tracing", () => mockTracingBase(mockTracingOverrides));

  const telemetry = await import("../public/js/telemetry.js");
  const Sentry = await import("@sentry/browser");
  return { telemetry, Sentry };
}

describe("telemetry additional: BrowserTracing fallback path", () => {
  afterEach(() => {
    jest.clearAllMocks();
    jest.restoreAllMocks();
  });

  it("uses new BrowserTracing when browserTracingIntegration is not a function", async () => {
    const { telemetry, Sentry } = await importTelemetryWithCustomMocks({
      // Explicitly remove browserTracingIntegration to exercise BrowserTracing path
      browserTracingIntegration: undefined,
      // Ensure profiling off
      browserProfilingIntegration: undefined,
      profilingIntegration: undefined,
    });

    const cfg = {
      sentry: {
        enabled: true,
        dsn: "https://example.invalid/abc123",
        traces_sample_rate: 0.2,
        profiles_sample_rate: 0, // ensure profiling integration is not added
      },
    };

    const ok = telemetry.initTelemetry(cfg);
    expect(ok).toBe(true);
    expect(Sentry.init).toHaveBeenCalledTimes(1);

    const initArg = Sentry.init.mock.calls[0][0];
    expect(Array.isArray(initArg.integrations)).toBe(true);
    // Verify at least one integration is a BrowserTracing instance with targets
    const tracingIntegration = initArg.integrations.find(
      (i) => i && i.opts && Array.isArray(i.opts.tracePropagationTargets),
    );
    expect(tracingIntegration).toBeTruthy();
    expect(tracingIntegration.opts.tracePropagationTargets.length).toBeGreaterThan(0);
  });
});

describe("telemetry additional: setSpanAttributes fallback to setAttribute", () => {
  it("invokes setAttribute per key when setAttributes is not available", async () => {
    jest.resetModules();
    const telemetry = await import("../public/js/telemetry.js");

    const span = {
      // No setAttributes -> should fallback to setAttribute per-key
      setAttribute: jest.fn(),
    };
    telemetry.setSpanAttributes(span, { a: 1, b: "two", c: false });

    // Called once per key
    expect(span.setAttribute).toHaveBeenCalledTimes(3);
    expect(span.setAttribute).toHaveBeenCalledWith("a", 1);
    expect(span.setAttribute).toHaveBeenCalledWith("b", "two");
    expect(span.setAttribute).toHaveBeenCalledWith("c", false);
  });
});

describe("telemetry additional: endSpan with status", () => {
  it("sets status then ends span when provided", async () => {
    jest.resetModules();
    const telemetry = await import("../public/js/telemetry.js");

    const span = {
      setStatus: jest.fn(),
      end: jest.fn(),
    };
    telemetry.endSpan(span, { status: "ok" });

    expect(span.setStatus).toHaveBeenCalledTimes(1);
    expect(span.setStatus).toHaveBeenCalledWith("ok");
    expect(span.end).toHaveBeenCalledTimes(1);
  });

  it("no-op when span has no end()", async () => {
    jest.resetModules();
    const telemetry = await import("../public/js/telemetry.js");

    const span = {
      setStatus: jest.fn(),
      // end is missing
    };
    // Should not throw
    telemetry.endSpan(span, { status: "ok" });
    expect(span.setStatus).not.toHaveBeenCalled();
  });
});

describe("telemetry additional: console formatting of Error objects", () => {
  const ORIGINALS = {};
  const METHODS = ["error"];

  function saveConsole() {
    METHODS.forEach((m) => {
      ORIGINALS[m] = console[m];
    });
  }

  function restoreConsole() {
    METHODS.forEach((m) => {
      if (ORIGINALS[m]) console[m] = ORIGINALS[m];
    });
  }

  beforeEach(() => {
    saveConsole();
  });

  afterEach(() => {
    restoreConsole();
    jest.clearAllMocks();
    jest.restoreAllMocks();
  });

  it("forwards Error objects to Sentry with formatted message 'Name: message'", async () => {
    const { telemetry, Sentry } = await importTelemetryWithCustomMocks({
      // Provide browserTracingIntegration to avoid class branch here
      browserTracingIntegration: jest.fn((opts) => ({
        name: "browserTracingIntegration",
        opts,
      })),
      browserProfilingIntegration: undefined,
      profilingIntegration: undefined,
    });

    telemetry.initTelemetry({
      sentry: { enabled: true, dsn: "https://dsn.invalid/1", traces_sample_rate: 0.2 },
    });

    const err = new Error("boom goes the dynamite");
    console.error(err);

    expect(Sentry.captureEvent).toHaveBeenCalled();
    const events = Sentry.captureEvent.mock.calls.map((c) => c[0]);
    const matching = events.find((e) =>
      typeof e?.message === "string" && e.message.includes("Error: boom goes the dynamite"),
    );
    expect(matching).toBeTruthy();
    expect(matching.level).toBe("error");
    expect(matching.logger).toBe("browser.console");
    expect(matching.extra.console_method).toBe("error");
    expect(Array.isArray(matching.extra.arguments)).toBe(true);
  });
});
