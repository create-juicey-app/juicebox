import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

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

function mockSentry(overrides = {}) {
  const base = {
    __esModule: true,
    init: jest.fn(),
    configureScope: (cb) => cb(SCOPE),
    browserTracingIntegration: jest.fn((opts) => ({
      name: "browserTracingIntegration",
      opts,
    })),
    browserProfilingIntegration: jest.fn(() => ({
      name: "browserProfilingIntegration",
    })),
    startSpan: jest.fn(async (_opts, cb) => {
      return await cb();
    }),
    startInactiveSpan: jest.fn((opts) => ({
      ...opts,
      setAttributes: jest.fn(),
      setAttribute: jest.fn(),
      end: jest.fn(),
      setStatus: jest.fn(),
    })),
    getActiveSpan: jest.fn(() => null),
    captureException: jest.fn(),
    captureEvent: jest.fn(),
  };
  return { ...base, ...overrides };
}

function mockTracing(overrides = {}) {
  const base = {
    __esModule: true,
    BrowserTracing: function BrowserTracing(opts) {
      this.opts = opts;
    },
  };
  return { ...base, ...overrides };
}

async function importTelemetryWithMocks(
  mockSentryOverrides = {},
  mockTracingOverrides = {},
) {
  jest.resetModules();
  resetScopeMocks();

  jest.mock("@sentry/browser", () => mockSentry(mockSentryOverrides));
  jest.mock("@sentry/tracing", () => mockTracing(mockTracingOverrides));

  const telemetry = await import("../public/js/telemetry.js");
  // Return both telemetry module and the mocked sentry module reference
  const Sentry = await import("@sentry/browser");
  return { telemetry, Sentry };
}

const ORIGINAL_CONSOLE = {};
const CONSOLE_METHODS = ["log", "info", "warn", "error", "debug", "trace"];

function saveConsole() {
  CONSOLE_METHODS.forEach((m) => {
    ORIGINAL_CONSOLE[m] = console[m];
  });
}

function restoreConsole() {
  CONSOLE_METHODS.forEach((m) => {
    if (ORIGINAL_CONSOLE[m]) {
      console[m] = ORIGINAL_CONSOLE[m];
    }
  });
}

describe("telemetry.js", () => {
  beforeEach(() => {
    saveConsole();
  });

  afterEach(() => {
    restoreConsole();
    jest.clearAllMocks();
    jest.restoreAllMocks();
  });

  it("initTelemetry returns false when no config or incomplete sentry config", async () => {
    const { telemetry, Sentry } = await importTelemetryWithMocks();

    expect(telemetry.isTelemetryEnabled()).toBe(false);
    expect(telemetry.getTelemetryClient()).toBeNull();

    const res1 = telemetry.initTelemetry(null);
    expect(res1).toBe(false);
    expect(telemetry.isTelemetryEnabled()).toBe(false);
    expect(telemetry.getTelemetryClient()).toBeNull();
    expect(Sentry.init).not.toHaveBeenCalled();

    const res2 = telemetry.initTelemetry({ sentry: { enabled: true } }); // missing dsn
    expect(res2).toBe(false);
    expect(telemetry.isTelemetryEnabled()).toBe(false);
    expect(telemetry.getTelemetryClient()).toBeNull();
    expect(Sentry.init).not.toHaveBeenCalled();
  });

  it("initTelemetry configures Sentry, sets tags and enables helpers; subsequent calls no-op", async () => {
    const { telemetry, Sentry } = await importTelemetryWithMocks();

    const cfg = {
      sentry: {
        enabled: true,
        dsn: "https://example.invalid/123",
        release: "1.2.3",
        environment: "test",
        traces_sample_rate: 0.42,
        profiles_sample_rate: 0.25,
        trace_propagation_targets: ["/api/.*", "^/upload", /\/list/],
      },
    };

    const initResult1 = telemetry.initTelemetry(cfg);
    expect(initResult1).toBe(true);
    expect(telemetry.isTelemetryEnabled()).toBe(true);
    expect(telemetry.getTelemetryClient()).not.toBeNull();
    // To be null, or to be .. Uh, not to be null
    // Sentry.init called once with expected pieces
    expect(Sentry.init).toHaveBeenCalledTimes(1);
    const initArg = Sentry.init.mock.calls[0][0];
    expect(initArg).toMatchObject({
      dsn: cfg.sentry.dsn,
      release: cfg.sentry.release,
      environment: cfg.sentry.environment,
      tracesSampleRate: cfg.sentry.traces_sample_rate,
      profilesSampleRate: cfg.sentry.profiles_sample_rate,
      autoSessionTracking: true,
      sendDefaultPii: false,
      enableTracing: true,
    });
    expect(initArg.tracesSampler).toEqual(expect.any(Function));
    // Integrations include browser tracing (from mocked browserTracingIntegration)
    expect(Array.isArray(initArg.integrations)).toBe(true);
    expect(initArg.integrations.length).toBeGreaterThanOrEqual(2);
    expect(Sentry.browserTracingIntegration).toHaveBeenCalledTimes(1);
    expect(Sentry.browserProfilingIntegration).toHaveBeenCalledTimes(1);

    // Tags and extras configured on scope
    expect(SCOPE.setTag).toHaveBeenCalledWith("service", "juicebox-frontend");
    expect(SCOPE.setTag).toHaveBeenCalledWith("runtime", "browser");
    expect(SCOPE.setTag).toHaveBeenCalledWith("profiling", "enabled");
    expect(SCOPE.setExtra).toHaveBeenCalledWith(
      "trace_propagation_targets",
      expect.any(Array),
    );
    expect(SCOPE.setExtra).toHaveBeenCalledWith(
      "profiles_sample_rate",
      cfg.sentry.profiles_sample_rate,
    );

    // Second init is no-op, still enabled and not re-initialized
    const initResult2 = telemetry.initTelemetry(cfg);
    expect(initResult2).toBe(true);
    expect(Sentry.init).toHaveBeenCalledTimes(1);
  });

  it("startSpan executes callback and uses Sentry when initialized, falls back when not", async () => {
    // Not initialized case
    {
      const { telemetry, Sentry } = await importTelemetryWithMocks();
      const out = await telemetry.startSpan(
        "my.span",
        { op: "x" },
        async () => 42,
      );
      expect(out).toBe(42);
      expect(Sentry.startSpan).not.toHaveBeenCalled();
    }
    // Initialized case
    {
      const { telemetry, Sentry } = await importTelemetryWithMocks();
      telemetry.initTelemetry({ sentry: { enabled: true, dsn: "x://" } });
      const out = await telemetry.startSpan(
        "upload",
        { op: "http.client" },
        async () => "ok",
      );
      expect(out).toBe("ok");
      expect(Sentry.startSpan).toHaveBeenCalledTimes(1);
      const [spanOpts] = Sentry.startSpan.mock.calls[0];
      expect(spanOpts).toMatchObject({ name: "upload", op: "http.client" });
    }
  });

  it("inactive span helpers: set attributes and end with status", async () => {
    const { telemetry } = await importTelemetryWithMocks();
    telemetry.initTelemetry({ sentry: { enabled: true, dsn: "x://" } });

    const span = telemetry.startInactiveSpan("custom.work", { op: "custom" });
    expect(span).toBeTruthy();
    expect(typeof span.setAttributes).toBe("function");
    expect(typeof span.end).toBe("function");
    expect(typeof span.setStatus).toBe("function");

    telemetry.setSpanAttributes(span, { a: 1, b: "two" });
    expect(span.setAttributes).toHaveBeenCalledWith({ a: 1, b: "two" });

    telemetry.endSpan(span, { status: "ok" });
    expect(span.setStatus).toHaveBeenCalledWith("ok");
    expect(span.end).toHaveBeenCalled();
  });

  it("captureException forwards to Sentry when enabled", async () => {
    const { telemetry, Sentry } = await importTelemetryWithMocks();
    telemetry.initTelemetry({ sentry: { enabled: true, dsn: "x://" } });

    const err = new Error("boom");
    telemetry.captureException(err, { phase: "test" });
    expect(Sentry.captureException).toHaveBeenCalledTimes(1);
    expect(Sentry.captureException).toHaveBeenCalledWith(
      err,
      expect.any(Function),
    );
  });

  it("getActiveSpan returns null by default", async () => {
    const { telemetry } = await importTelemetryWithMocks();
    expect(telemetry.getActiveSpan()).toBeNull();
  });

  it("console forwarding captures console events to Sentry after init", async () => {
    const { telemetry, Sentry } = await importTelemetryWithMocks();
    telemetry.initTelemetry({ sentry: { enabled: true, dsn: "x://" } });

    console.warn("something happened", { foo: "bar" });
    console.error("bad thing");
    console.log("info"); // level 'info'

    // Ensure captureEvent was called for forwarded logs
    expect(Sentry.captureEvent).toHaveBeenCalled();
    const events = Sentry.captureEvent.mock.calls.map((c) => c[0]);

    const levels = events.map((e) => e.level);
    expect(levels).toEqual(
      expect.arrayContaining(["warning", "error", "info"]),
    );

    // One of the messages should match our warn
    expect(
      events.some((e) => String(e.message).includes("something happened")),
    ).toBe(true);
    // Logger should be annotated
    events.forEach((e) => {
      expect(e.logger).toBe("browser.console");
      expect(e.extra).toBeDefined();
      expect(e.extra.console_method).toBeDefined();
      expect(Array.isArray(e.extra.arguments)).toBe(true);
    });
  });
});
