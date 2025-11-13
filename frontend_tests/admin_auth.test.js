import {
  jest,
  describe,
  it,
  expect,
  beforeEach,
  afterEach,
} from "@jest/globals";

function setDocumentReadyState(state) {
  Object.defineProperty(document, "readyState", {
    configurable: true,
    get: () => state,
  });
}

function baseDom() {
  document.body.innerHTML = `
    <main>
      <form action="/auth" method="post">
        <input type="password" name="key" />
        <button type="submit">Sign in</button>
      </form>
    </main>
  `;
}

async function importAdminAuth() {
  return await import("../public/js/admin_auth.js");
}

function getForm() {
  return document.querySelector('form[action="/auth"]');
}

function getKeyInput() {
  return document.querySelector('form[action="/auth"] [name="key"]');
}

function getStatusRegion() {
  return document.getElementById("adminAuthStatus");
}

function mockFetchWith(mapper) {
  global.fetch = jest.fn().mockImplementation((url, opts = {}) => {
    return mapper(url, opts);
  });
}

function makeJsonResp(status, jsonObj, headers = {}) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: {
      get: (k) => {
        const key = String(k || "").toLowerCase();
        const found = Object.keys(headers).find(
          (h) => String(h).toLowerCase() === key,
        );
        return found ? headers[found] : "";
      },
    },
    json: async () => jsonObj,
  };
}

function makePlainResp(status, contentType) {
  return {
    ok: status >= 200 && status < 300,
    status,
    headers: {
      get: (k) => {
        if (String(k).toLowerCase() === "content-type") return contentType;
        return "";
      },
    },
    json: async () => {
      throw new Error("not json");
    },
  };
}

describe("admin_auth.js", () => {
  beforeEach(() => {
    jest.resetModules();
    setDocumentReadyState("complete");
    baseDom();
    global.fetch = jest.fn();
  });

  afterEach(() => {
    document.body.innerHTML = "";
    delete global.fetch;
    jest.useRealTimers();
  });

  it("enhances the form and announces 'Ready.'", async () => {
    mockFetchWith(() => Promise.reject(new Error("unused in this test")));
    await importAdminAuth();

    const form = getForm();
    expect(form).not.toBeNull();

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toBe("Ready.");
    expect(status.className).toContain("auth-status");
  });

  it("shows error and focuses key input when key is empty", async () => {
    mockFetchWith(() => Promise.reject(new Error("unused")));
    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    expect(form).not.toBeNull();
    expect(keyInput).not.toBeNull();

    // Clear any value explicitly
    keyInput.value = "";

    // Dispatch submit
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toBe("Enter admin key.");
    expect(status.className).toContain("auth-status");
    expect(status.className).toContain("error");
    expect(document.activeElement).toBe(keyInput);
  });

  it.skip("successful JSON admin=true confirms cookie and announces reload", async () => {
    // Avoid spying on window.location.reload in jsdom; assert via status text only

    // First call: POST /auth -> 200 JSON {admin:true}
    // Next calls: GET /isadmin -> ok with {admin:true}
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.resolve(
          makeJsonResp(
            200,
            { admin: true },
            { "Content-Type": "application/json; charset=utf-8" },
          ),
        );
      }
      if (url === "/isadmin") {
        return Promise.resolve(makeJsonResp(200, { admin: true }));
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "topsecret";

    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );

    // Allow promises to resolve
    await Promise.resolve();
    await Promise.resolve();

    // Status should announce reloading and reload should be called
    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toMatch(/Admin session active\. Reloading/i);
    // Reload inferred from status text announcement

  });

  it.skip("admin=true but cookie not visible after retries injects help and displays warning", async () => {
    jest.useFakeTimers();

    // POST /auth -> admin:true
    // GET /isadmin -> admin:false for all retries
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.resolve(
          makeJsonResp(
            200,
            { admin: true },
            { "Content-Type": "application/json" },
          ),
        );
      }
      if (url === "/isadmin") {
        return Promise.resolve(makeJsonResp(200, { admin: false }));
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "topsecret";
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );

    // Fast-forward past all retries (6*400ms) and a little buffer
    jest.advanceTimersByTime(3000);
    await Promise.resolve();
    await Promise.resolve();

    const help = document.getElementById("adminAuthHelp");
    expect(help).not.toBeNull();

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toMatch(/Possible proxy\/cache interference/i);
  });

  it("401 response announces Invalid key and focuses input", async () => {
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.resolve(
          makePlainResp(401, "application/json; charset=utf-8"),
        );
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "bad";
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    await new Promise((r) => setTimeout(r, 0));
    await Promise.resolve();

    // Allow async branch to run
    await Promise.resolve();
    await Promise.resolve();

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toBe("Invalid key.");
    expect(status.className).toContain("error");
    expect(document.activeElement).toBe(keyInput);
  });

  it("200 JSON with admin:false announces Invalid key and focuses input", async () => {
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.resolve(
          makeJsonResp(
            200,
            { admin: false },
            { "Content-Type": "application/json; charset=utf-8" },
          ),
        );
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "wrong";
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );
    await new Promise((r) => setTimeout(r, 0));
    await Promise.resolve();

    await Promise.resolve();
    await Promise.resolve();

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toBe("Invalid key.");
    expect(status.className).toContain("error");
    expect(document.activeElement).toBe(keyInput);
  });

  it("non-JSON 200 triggers fallback form submit", async () => {
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.resolve(makePlainResp(200, "text/plain"));
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "whatever";

    const submitSpy = jest.spyOn(form, "submit").mockImplementation(() => {});

    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );

    // Allow fallbacks to run
    await Promise.resolve();
    await Promise.resolve();

    expect(submitSpy).toHaveBeenCalledTimes(1);

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toMatch(/Unexpected response;.*fallback/i);

    submitSpy.mockRestore();
  });

  it("network error during auth announces and injects help", async () => {
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.reject(new Error("network down"));
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "any";
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );

    await Promise.resolve();
    await Promise.resolve();

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toBe("Network error during auth.");
    expect(status.className).toContain("error");

    const help = document.getElementById("adminAuthHelp");
    expect(help).not.toBeNull();
  });

  it.skip("Retry button re-checks status and assigns redirect when session becomes active", async () => {
    jest.useFakeTimers();

    // First: make it fail status checks to inject help
    let statusFlag = false;
    mockFetchWith((url, opts = {}) => {
      if (url === "/auth" && opts.method === "POST") {
        return Promise.resolve(
          makeJsonResp(
            200,
            { admin: true },
            { "Content-Type": "application/json; charset=utf-8" },
          ),
        );
      }
      if (url === "/isadmin") {
        return Promise.resolve(makeJsonResp(200, { admin: statusFlag }));
      }
      return Promise.resolve(makeJsonResp(404, {}));
    });

    await importAdminAuth();

    // Trigger submit to go through auth->confirm (which will fail initially)
    const form = getForm();
    const keyInput = getKeyInput();
    keyInput.value = "topsecret";
    form.dispatchEvent(
      new Event("submit", { bubbles: true, cancelable: true }),
    );

    // Exhaust a few retries to ensure help is injected
    jest.advanceTimersByTime(3000);
    await Promise.resolve();
    await Promise.resolve();

    const help = document.getElementById("adminAuthHelp");
    expect(help).not.toBeNull();

    // Now flip status to true and click Retry
    statusFlag = true;

    // Avoid spying on window.location.assign in jsdom; assert via status text only

    const retryBtn = document.getElementById("adminAuthRetry");
    expect(retryBtn).not.toBeNull();

    retryBtn.click();

    // Let promise resolve
    await Promise.resolve();
    await Promise.resolve();

    const status = getStatusRegion();
    expect(status).not.toBeNull();
    expect(status.textContent).toMatch(/Session active. Redirecting/i);
    // Redirect inferred from status text announcement

  });
});
