// Web Worker for SHA-256 file hashing
self.onmessage = async function (e) {
  const file = e.data;
  try {
    const buffer = await file.arrayBuffer();
    const hashBuffer = await crypto.subtle.digest("SHA-256", buffer);
    const hashArray = Array.from(new Uint8Array(hashBuffer));
    const hex = hashArray.map((b) => b.toString(16).padStart(2, "0")).join("");
    self.postMessage({ hash: hex });
  } catch (err) {
    self.postMessage({ error: err.message || "Hashing failed" });
  }
};
