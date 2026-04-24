/**
 * Copy text to the clipboard.
 *
 * `navigator.clipboard` is only available in secure contexts (HTTPS or
 * localhost). On a self-hosted gateway accessed via `http://192.168.x.x`
 * the API is undefined and the native call throws. This helper transparently
 * falls back to the legacy `document.execCommand('copy')` path via a
 * temporary off-screen textarea.
 */
export function copyText(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    return navigator.clipboard.writeText(text).catch(() => fallbackCopy(text));
  }
  return fallbackCopy(text);
}

function fallbackCopy(text: string): Promise<void> {
  return new Promise((resolve, reject) => {
    const ta = document.createElement("textarea");
    ta.value = text;
    ta.style.cssText = "position:fixed;left:-9999px;top:-9999px;opacity:0";
    document.body.appendChild(ta);
    ta.select();
    try {
      const ok = document.execCommand("copy");
      if (ok) resolve();
      else reject(new Error("execCommand('copy') returned false"));
    } catch (e) {
      reject(e);
    } finally {
      document.body.removeChild(ta);
    }
  });
}
