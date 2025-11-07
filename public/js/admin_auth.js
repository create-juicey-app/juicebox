/**
 * Admin auth helper script.
 *
 * Purpose:
 *  - Submit the admin key to the new /auth/json endpoint.
 *  - Avoid Cloudflare / proxy stripping of 303 + Set-Cookie by using a 200 JSON flow.
 *  - Provide accessible feedback and retry logic.
 *
 * Usage:
 *  Include this script only on /auth pages (e.g. <script type="module" src="/js/admin_auth.js"></script>)
 *
 * Markup assumptions (progressive enhancement):
 *  - A <form method="post" action="/auth"> containing an <input name="key">
 *  - We do NOT remove existing action; fallback submission still works if JS fails.
 *
 * Behavior:
 *  - Intercepts submit.
 *  - Posts key via fetch() to /auth/json (application/x-www-form-urlencoded).
 *  - On success ({"admin": true}) polls /isadmin to confirm cookie presence.
 *  - Redirects to / (or configurable target) once confirmed.
 *  - If cookie not visible after a few retries, offers a manual fallback link & explains potential proxy interference.
 */

const JSON_ENDPOINT = "/auth/json";
const STATUS_ENDPOINT = "/isadmin";
const REDIRECT_TARGET = "/"; // could be changed to "/admin/files" etc.
const MAX_STATUS_RETRIES = 6;
const STATUS_RETRY_INTERVAL_MS = 400;

function $(sel, root = document) {
  return root.querySelector(sel);
}

function createStatusRegion() {
  let region = $("#adminAuthStatus");
  if (!region) {
    region = document.createElement("div");
    region.id = "adminAuthStatus";
    region.className = "auth-status";
    region.setAttribute("role", "status");
    region.setAttribute("aria-live", "polite");
    const form = getAuthForm();
    if (form) {
      form.parentNode.insertBefore(region, form.nextSibling);
    } else {
      document.body.appendChild(region);
    }
  }
  return region;
}

function announce(msg, cls = "") {
  const region = createStatusRegion();
  region.textContent = msg;
  region.className = "auth-status " + cls;
  if (window.DEBUG_LOGS) console.log("[admin-auth]", msg);
}

function getAuthForm() {
  return document.querySelector('form[action="/auth"], form[action="/auth/"]');
}

async function postFormEncoded(url, data) {
  const body = new URLSearchParams();
  for (const [k, v] of Object.entries(data)) {
    body.append(k, v);
  }
  const resp = await fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/x-www-form-urlencoded",
      "Accept": "application/json, text/plain, */*",
    },
    body: body.toString(),
    credentials: "include",
    redirect: "manual",
    cache: "no-store",
  });
  return resp;
}

async function checkAdminStatus() {
  try {
    const r = await fetch(STATUS_ENDPOINT, {
      credentials: "include",
      cache: "no-store",
    });
    if (!r.ok) return false;
    const j = await r.json().catch(() => ({}));
    return !!j.admin;
  } catch {
    return false;
  }
}

async function confirmSessionWithRetries() {
  for (let i = 0; i < MAX_STATUS_RETRIES; i++) {
    const ok = await checkAdminStatus();
    if (ok) return true;
    await new Promise((res) => setTimeout(res, STATUS_RETRY_INTERVAL_MS));
  }
  return false;
}

function injectHelpIfNeeded() {
  if ($("#adminAuthHelp")) return;
  const div = document.createElement("div");
  div.id = "adminAuthHelp";
  div.className = "auth-help small text-subtle";
  div.innerHTML = `
    <p>
      Having trouble? The secure session cookie might be blocked by an intermediate cache or
      a proxy rewriting the redirect. Try:
    </p>
    <ul>
      <li>Hard refresh (Ctrl/Cmd+Shift+R)</li>
      <li>Ensure no aggressive "Cache Everything" rule on /auth</li>
      <li>Disable any privacy extensions blocking cookies</li>
      <li>Retry below</li>
    </ul>
    <p>
      <button type="button" id="adminAuthRetry" class="secondary">Retry Status Check</button>
      <button type="button" id="adminAuthFallbackSubmit" class="">Fallback Form Submit</button>
    </p>
  `;
  const form = getAuthForm();
  if (form) form.parentNode.insertBefore(div, form.nextSibling);
  $("#adminAuthRetry")?.addEventListener("click", async () => {
    announce("Re-checking session…");
    const ok = await confirmSessionWithRetries();
    if (ok) {
      announce("Session active. Redirecting…");
      window.location.assign(REDIRECT_TARGET);
    } else {
      announce("Still not seeing session cookie.", "error");
    }
  });
  $("#adminAuthFallbackSubmit")?.addEventListener("click", () => {
    const f = getAuthForm();
    if (f) {
      announce("Submitting fallback form (redirect flow)…");
      f.removeEventListener("submit", interceptSubmit, { capture: true });
      f.submit();
    }
  });
}

async function interceptSubmit(e) {
  try {
    e.preventDefault();
    const form = e.currentTarget;
    const keyInput = form.querySelector('[name="key"]');
    const key = keyInput ? keyInput.value.trim() : "";
    if (!key) {
      announce("Enter admin key.", "error");
      keyInput?.focus();
      return;
    }
    announce("Authenticating…");
    const resp = await postFormEncoded(JSON_ENDPOINT, { key });
    const ct = resp.headers.get("Content-Type") || "";
    if (resp.status === 200 && ct.includes("application/json")) {
      let data = {};
      try {
        data = await resp.json();
      } catch {
        // ignore
      }
      if (data.admin) {
        announce("Session issued, verifying cookie…");
        const ok = await confirmSessionWithRetries();
        if (ok) {
          announce("Admin session active. Redirecting…");
          window.location.assign(REDIRECT_TARGET);
          return;
        } else {
          announce(
            "Token created but cookie not visible. Possible proxy/cache interference.",
            "warn",
          );
          injectHelpIfNeeded();
          return;
        }
      } else {
        announce("Invalid key.", "error");
        keyInput?.focus();
        return;
      }
    } else if (resp.status === 401) {
      announce("Invalid key.", "error");
      keyInput?.focus();
      return;
    } else {
      // Non-JSON or unexpected status: fallback to normal form submit
      announce("Unexpected response; attempting fallback redirect flow…");
      form.removeEventListener("submit", interceptSubmit, { capture: true });
      form.submit();
    }
  } catch (err) {
    announce("Network error during auth.", "error");
    injectHelpIfNeeded();
    if (window.DEBUG_LOGS) console.error("[admin-auth] error", err);
  }
}

function enhanceForm() {
  const form = getAuthForm();
  if (!form) return;
  if (form.dataset.enhanced === "1") return;
  form.dataset.enhanced = "1";
  form.addEventListener("submit", interceptSubmit, { capture: true });
  announce("Ready.");
}

function init() {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", enhanceForm, { once: true });
  } else {
    enhanceForm();
  }
}

init();
export {}; // Ensure ES module context (tree-shakable)
