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
  const cookies = await collectCookiesForUrl(targetUrl);
  const resp = await apiFetch("/api/v1/downloads", {
    method: "POST",
    body: JSON.stringify({
      urls: [targetUrl],
      start_immediately: true,
      cookies,
      user_agent: navigator.userAgent,
      referer: refererUrl || targetUrl,
      open_in_ui: openInUi
    })
  });
  if (!resp.ok) {
    throw new Error(`API status ${resp.status}`);
  }
  return resp.json();
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

function matchesInterceptFilter(item, settings) {
  const minBytes = Math.max(0, Number(settings.minSizeMb) || 0) * 1024 * 1024;
  if (typeof item.fileSize === "number" && item.fileSize >= 0 && item.fileSize < minBytes) {
    return false;
  }
  const prefixes = Array.isArray(settings.mimePrefixes)
    ? settings.mimePrefixes.map((p) => p.trim().toLowerCase()).filter(Boolean)
    : [];
  if (prefixes.length === 0) {
    return true;
  }
  const mime = (item.mime || "").toLowerCase();
  if (!mime) {
    return true;
  }
  return prefixes.some((prefix) => mime.startsWith(prefix));
}

if (ext.downloads && ext.downloads.onCreated) {
  ext.downloads.onCreated.addListener(async (item) => {
    try {
      const settings = await getSettings();
      if (!settings.token || !settings.interceptDownloads) {
        return;
      }
      const targetUrl = item.finalUrl || item.url;
      if (!targetUrl || isLocalLoopbackUrl(targetUrl)) {
        return;
      }
      if (!matchesInterceptFilter(item, settings)) {
        return;
      }
      await sendDownloadToRuscello(targetUrl, item.referrer || null, false);
      await ext.downloads.cancel(item.id);
      if (typeof ext.downloads.erase === "function") {
        await ext.downloads.erase({ id: item.id });
      }
    } catch (_) {
      // Leave the browser download intact if Ruscello is unreachable or rejects.
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
