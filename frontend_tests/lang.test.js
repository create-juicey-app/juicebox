describe("JBLang helper", () => {
  let originalHref;
  let navigations;

  beforeEach(() => {
    jest.resetModules();
    originalHref = window.location.href;
    window.history.replaceState(null, "", "/");
    window.localStorage.clear();
    navigations = [];
    window.__JBLangNavigate = (url, mode) => {
      const next = new URL(url, window.location.href);
      window.history.replaceState(
        null,
        "",
        next.pathname + next.search + next.hash
      );
      navigations.push({ url, mode });
    };
    document.body.innerHTML = `
      <a id="navFaq" href="/faq">FAQ</a>
      <a id="fileLink" href="/f/abc" data-lang-skip="true">File</a>
    `;
  });

  afterEach(() => {
    window.history.replaceState(null, "", originalHref);
    delete window.__JBLangNavigate;
    document.body.innerHTML = "";
  });

  async function loadHelper() {
    await import("../public/js/lang.js");
  }

  it("stores selection and rewrites internal links", async () => {
    await loadHelper();
    const result = window.JBLang.applyLanguage("fr", { replace: true });
    expect(result).toBe("fr");
    expect(window.localStorage.getItem("jb.lang")).toBe("fr");
    expect(navigations).toHaveLength(1);
    expect(navigations[0].mode).toBe("replace");
    expect(navigations[0].url).toContain("?lang=fr");
    window.JBLang.rewriteLinks();
    expect(document.getElementById("navFaq").getAttribute("href")).toBe(
      "/faq?lang=fr"
    );
    expect(document.getElementById("fileLink").getAttribute("href")).toBe(
      "/f/abc"
    );
    expect(window.location.href).toContain("?lang=fr");
  });

  it("ignores unsupported language codes", async () => {
    await loadHelper();
    const result = window.JBLang.applyLanguage("zz");
    expect(result).toBeNull();
    expect(window.localStorage.getItem("jb.lang")).toBeNull();
    expect(navigations).toHaveLength(0);
  });
});
