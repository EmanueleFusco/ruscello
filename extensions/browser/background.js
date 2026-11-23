const ext = globalThis.browser ?? globalThis.chrome;

export const DEFAULT_SETTINGS = {
  apiPort: 9667,
  token: "",
  interceptDownloads: false,
  minSizeMb: 0,
  mimePrefixes: ["application/", "video/", "audio/"],
  language: "auto"
};

const MENU_DOWNLOAD_ID = "ruscello-download-target";
const MENU_ANALYZE_ID = "ruscello-analyze-page";

export async function getSettings() {
  const stored = await ext.storage.local.get(DEFAULT_SETTINGS);
  return Object.assign({}, DEFAULT_SETTINGS, stored);
}

export async function saveSettings(patch) {
  await ext.storage.local.set(patch);
  return getSettings();
}

function apiUrl(settings, path) {
  const port = Number(settings.apiPort) || 9667;
  return `http://127.0.0.1:${port}${path}`;
}

export async function apiFetch(path, options = {}, overrideSettings = null) {
  const settings = overrideSettings || (await getSettings());
  const headers = Object.assign({}, options.headers || {});
  if (settings.token) {
    headers.Authorization = `Bearer ${settings.token}`;
  }
  if (options.body && !headers["Content-Type"]) {
    headers["Content-Type"] = "application/json";
  }
  return fetch(apiUrl(settings, path), Object.assign({}, options, { headers }));
}

export async function collectCookiesForUrl(url) {
  if (!url || !ext.cookies || typeof ext.cookies.getAll !== "function") {
    return [];
  }
  try {
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      return [];
    }
    const items = await ext.cookies.getAll({ url });
    return items.map((c) => `${c.name}=${c.value}`);
  } catch (_) {
    return [];
  }
}

function extractFirstUrlFromSelection(text) {
  if (!text) {
    return null;
  }
  const match = text.match(/(?:https?:\/\/|magnet:\?)[^\s<>"']+/i);
  return match ? match[0] : null;
}

function isLocalLoopbackUrl(rawUrl) {
  try {
    const u = new URL(rawUrl);
    if (u.protocol === "magnet:") {
      return false;
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") {
      return true;
    }
    return ["127.0.0.1", "localhost", "::1", "[::1]"].includes(u.hostname);
  } catch (_) {
    return true;
  }
}

export async function sendDownloadToRuscello(targetUrl, refererUrl = null, openInUi = false) {
  const response = await submitDownload(targetUrl, refererUrl, { openInUi });
  return response.json();
}

async function submitDownload(targetUrl, refererUrl, {
  openInUi = false,
  startImmediately = true,
  settings = null
} = {}) {
  const cookies = await collectCookiesForUrl(targetUrl);
  const resp = await apiFetch("/api/v1/downloads", {
    method: "POST",
    signal: settings ? AbortSignal.timeout(5000) : undefined,
    body: JSON.stringify({
      urls: [targetUrl],
      start_immediately: startImmediately,
      cookies,
      user_agent: navigator.userAgent,
      referer: refererUrl || targetUrl,
      open_in_ui: openInUi
    })
  }, settings);
  if (!resp.ok) {
    throw new Error(`API status ${resp.status}`);
  }
  return resp;
}

export async function analyzePageWithLinkGrabber(tabUrl, openInUi = true) {
  const cookies = await collectCookiesForUrl(tabUrl);
  const resp = await apiFetch("/api/v1/linkgrabber/analyze", {
    method: "POST",
    body: JSON.stringify({
      url: tabUrl,
      cookies,
      user_agent: navigator.userAgent,
      referer: tabUrl,
      open_in_ui: openInUi
    })
  });
  if (!resp.ok) {
    throw new Error(`API status ${resp.status}`);
  }
  return resp.json();
}

export async function forwardDomainCookies(tabUrl) {
  const cookies = await collectCookiesForUrl(tabUrl);
  const resp = await apiFetch("/api/v1/cookies", {
    method: "POST",
    body: JSON.stringify({
      url: tabUrl,
      cookies,
      user_agent: navigator.userAgent,
      referer: tabUrl
    })
  });
  if (!resp.ok) {
    throw new Error(`API status ${resp.status}`);
  }
  return resp.json();
}

export async function requestPairing(pin = "") {
  const clientName = navigator.userAgent.includes("Firefox")
    ? "Firefox Extension"
    : "Chrome Extension";
  const settings = await getSettings();
  const payload = { client_name: clientName };
  if (pin && pin.trim()) {
    payload.pin = pin.trim();
  }
  const resp = await fetch(apiUrl(settings, "/api/v1/pair/request"), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload)
  });
  if (!resp.ok && resp.status !== 202) {
    throw new Error(`Pair status ${resp.status}`);
  }
  const data = await resp.json();
  if (data.status === "paired" && data.token) {
    await saveSettings({ token: data.token });
  }
  return data;
}

export async function pollPairingStatus(requestId) {
  const resp = await apiFetch(`/api/v1/pair/status/${encodeURIComponent(requestId)}`, {
    method: "GET"
  });
  if (!resp.ok) {
    throw new Error(`Poll status ${resp.status}`);
  }
  const data = await resp.json();
  if (data.status === "paired" && data.token) {
    await saveSettings({ token: data.token });
  }
  return data;
}

function installContextMenus() {
  if (!ext.contextMenus) {
    return;
  }
  ext.contextMenus.removeAll(() => {
    ext.contextMenus.create({
      id: MENU_DOWNLOAD_ID,
      title: ext.i18n.getMessage("menuDownload") || "Download with Ruscello",
      contexts: ["link", "selection"]
    });
    ext.contextMenus.create({
      id: MENU_ANALYZE_ID,
      title: ext.i18n.getMessage("menuAnalyze") || "Analyze page with LinkGrabber",
      contexts: ["page"]
    });
  });
}

if (ext.runtime && ext.runtime.onInstalled) {
  ext.runtime.onInstalled.addListener(installContextMenus);
}
if (ext.runtime && ext.runtime.onStartup) {
  ext.runtime.onStartup.addListener(installContextMenus);
}

if (ext.contextMenus && ext.contextMenus.onClicked) {
  ext.contextMenus.onClicked.addListener(async (info, tab) => {
    try {
      if (info.menuItemId === MENU_DOWNLOAD_ID) {
        const target = info.linkUrl || extractFirstUrlFromSelection(info.selectionText);
        if (target) {
          await sendDownloadToRuscello(target, (tab && tab.url) || info.pageUrl, false);
        }
      } else if (info.menuItemId === MENU_ANALYZE_ID) {
        const pageUrl = (tab && tab.url) || info.pageUrl;
        if (pageUrl) {
          await analyzePageWithLinkGrabber(pageUrl, true);
        }
      }
    } catch (_) {}
  });
}

function interceptDecision(item, settings) {
  const minBytes = Math.max(0, Number(settings.minSizeMb) || 0) * 1024 * 1024;
  // Chrome can initially report zero for an unknown size. A positive total is
  // still usable when the file size has not been populated.
  const size = [item.fileSize, item.totalBytes].find((bytes) => Number.isFinite(bytes) && bytes > 0);
  const prefixes = Array.isArray(settings.mimePrefixes)
    ? settings.mimePrefixes.map((p) => p.trim().toLowerCase()).filter(Boolean)
    : [];
  const mime = (item.mime || "").toLowerCase();
  if ((size !== undefined && size < minBytes) ||
      (mime && prefixes.length > 0 && !prefixes.some((prefix) => mime.startsWith(prefix)))) {
    return false;
  }
  return (minBytes > 0 && size === undefined) || (prefixes.length > 0 && !mime) ? null : true;
}

async function eligibleDownload(item, settings) {
  const deadline = Date.now() + 1500;
  while (item && item.state === "in_progress" && !item.paused) {
    const decision = interceptDecision(item, settings);
    if (decision === false) {
      return null;
    }
    // A browser can interrupt the request before any partial data can be resumed.
    if (decision === true && item.bytesReceived > 0) {
      return item;
    }
    if (Date.now() >= deadline) {
      break;
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
    [item] = await ext.downloads.search({ id: item.id });
  }
  return null;
}

async function supportsResumingInFirefox(url) {
  if (typeof ext.runtime?.getBrowserInfo !== "function") {
    return true;
  }
  // Firefox implements pause as a cancellation that must retain resumable data.
  const response = await fetch(url, {
    credentials: "include",
    headers: { Range: "bytes=0-0" },
    redirect: "error",
    signal: AbortSignal.timeout(500)
  });
  try {
    const etag = response.headers.get("ETag") || "";
    return response.status === 206 &&
      /^bytes 0-0\/[1-9]\d*$/i.test(response.headers.get("Content-Range") || "") &&
      (/^"[^"]*"$/.test(etag) || Boolean(response.headers.get("Last-Modified")));
  } finally {
    await response.body?.cancel();
  }
}

async function stageBrowserDownload(targetUrl, referer, settings) {
  const response = await submitDownload(targetUrl, referer, {
    startImmediately: false,
    settings
  });
  const body = await response.text();
  const result = JSON.parse(body);
  // Older versions send the ID as a JSON number. Read its digits before JavaScript rounds it.
  const id = result.added_id_strings?.[0] || body.match(/"added_ids"\s*:\s*\[\s*(\d+)\s*\]/)?.[1];
  if (typeof id !== "string" || !/^\d+$/.test(id)) {
    throw new Error("The desktop did not return an exact download ID");
  }
  return "/api/v1/downloads/" + id;
}

async function updateStagedDownload(path, method, settings) {
  const response = await apiFetch(path, { method, signal: AbortSignal.timeout(5000) }, settings);
  if (!response.ok) {
    throw new Error("API status " + response.status);
  }
}

function isCancelled(item) {
  return item.state === "interrupted" && !item.canResume &&
    (!item.paused || item.error === "USER_CANCELED");
}

if (ext.downloads && ext.downloads.onCreated) {
  ext.downloads.onCreated.addListener(async (item) => {
    let resumeBrowser = false;
    let discardStaged = false;
    let staged;
    let settings;
    try {
      settings = await getSettings();
      if (!settings.token || !settings.interceptDownloads) {
        return;
      }
      const eligible = await eligibleDownload(item, settings);
      const targetUrl = eligible && (eligible.finalUrl || eligible.url);
      if (!targetUrl || isLocalLoopbackUrl(targetUrl) || !(await supportsResumingInFirefox(targetUrl))) {
        return;
      }
      await ext.downloads.pause(item.id);
      resumeBrowser = true;
      const [paused] = await ext.downloads.search({ id: item.id });
      if (!paused || paused.state === "complete") {
        resumeBrowser = false;
        return;
      }
      // Firefox reports a paused item as interrupted; Chromium keeps it in progress.
      if (!paused.paused || (paused.state !== "in_progress" && paused.state !== "interrupted")) {
        return;
      }
      staged = await stageBrowserDownload(targetUrl, paused.referrer || null, settings);
      // A pause can race with finalization. Keep the desktop job paused until the original is cancelled.
      resumeBrowser = false;
      try { await ext.downloads.cancel(item.id); } catch (_) {}
      const [native] = await ext.downloads.search({ id: item.id });
      if (!native) return;
      if (!isCancelled(native)) {
        resumeBrowser = native.state !== "complete" && Boolean(native.paused || native.canResume);
        discardStaged = true;
        return;
      }
      // If activation fails, the staged job remains available for a later resume.
      await updateStagedDownload(staged + "/resume", "POST", settings);
      if (typeof ext.downloads.erase === "function") {
        await ext.downloads.erase({ id: item.id });
      }
    } catch (_) {
      // A finished browser download can no longer be paused or cancelled.
    } finally {
      if (resumeBrowser) {
        try { await ext.downloads.resume(item.id); } catch (_) {}
      }
      if (discardStaged) {
        try { await updateStagedDownload(staged + "?delete_files=false", "DELETE", settings); } catch (_) {}
      }
    }
  });
}

if (ext.runtime && ext.runtime.onMessage) {
  ext.runtime.onMessage.addListener((message, _sender, sendResponse) => {
    if (!message || typeof message !== "object") {
      return false;
    }
    (async () => {
      switch (message.type) {
        case "health": {
          const resp = await apiFetch("/api/v1/health");
          return resp.json();
        }
        case "pair":
          return requestPairing(message.pin || "");
        case "pair-poll":
          return pollPairingStatus(message.requestId);
        case "send-url":
          return sendDownloadToRuscello(message.url, message.referer || null, Boolean(message.openInUi));
        case "analyze-tab":
          return analyzePageWithLinkGrabber(message.url, true);
        case "forward-cookies":
          return forwardDomainCookies(message.url);
        case "toggle-alt-speed": {
          const resp = await apiFetch("/api/v1/speed/alternate", {
            method: "POST",
            body: "{}"
          });
          return resp.json();
        }
        default:
          return { error: "unknown_message" };
      }
    })()
      .then((data) => sendResponse({ ok: true, data }))
      .catch((error) => sendResponse({ ok: false, error: String(error) }));
    return true;
  });
}
