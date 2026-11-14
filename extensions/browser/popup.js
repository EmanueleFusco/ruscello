import { resolveLang, t } from "./i18n.js";

const ext = globalThis.browser ?? globalThis.chrome;

const DEFAULT_PREFS = {
  apiPort: 9667,
  token: "",
  interceptDownloads: false,
  language: "auto"
};

let lang = "en";
let prefs = Object.assign({}, DEFAULT_PREFS);
let pollTimer = null;

const el = {
  connPill: document.getElementById("conn-pill"),
  pairSection: document.getElementById("pair-section"),
  pinInput: document.getElementById("pin-input"),
  pairBtn: document.getElementById("pair-btn"),
  actionsSection: document.getElementById("actions-section"),
  interceptLabel: document.getElementById("intercept-label"),
  interceptToggle: document.getElementById("intercept-toggle"),
  interceptDesc: document.getElementById("intercept-desc"),
  sendUrlForm: document.getElementById("send-url-form"),
  sendUrlInput: document.getElementById("send-url-input"),
  sendUrlBtn: document.getElementById("send-url-btn"),
  analyzeTabBtn: document.getElementById("analyze-tab-btn"),
  cookiesBtn: document.getElementById("cookies-btn"),
  altSpeedBtn: document.getElementById("alt-speed-btn"),
  disconnectBtn: document.getElementById("disconnect-btn"),
  feedbackMsg: document.getElementById("feedback-msg"),
  optionsBtn: document.getElementById("options-btn"),
  langBtn: document.getElementById("lang-btn")
};

function showFeedback(text, isError = false) {
  el.feedbackMsg.textContent = text;
  el.feedbackMsg.classList.toggle("err", isError);
  el.feedbackMsg.classList.remove("hidden");
}

function applyTranslations() {
  el.pinInput.placeholder = t(lang, "pairPinPlaceholder");
  el.pairBtn.textContent = t(lang, "pairOneClick");
  el.interceptLabel.textContent = t(lang, "interceptToggle");
  el.interceptDesc.textContent = t(lang, "interceptDesc");
  el.sendUrlInput.placeholder = t(lang, "sendUrlPlaceholder");
  el.sendUrlBtn.textContent = t(lang, "sendUrlBtn");
  el.analyzeTabBtn.textContent = t(lang, "analyzeTab");
  el.cookiesBtn.textContent = t(lang, "forwardCookies");
  el.altSpeedBtn.textContent = "🐢 " + t(lang, "altSpeedBtn");
  el.disconnectBtn.textContent = t(lang, "disconnect");
  el.optionsBtn.textContent = t(lang, "openOptions");
  el.langBtn.textContent = lang === "it" ? "EN" : "IT";
}

function sendMsg(payload) {
  return ext.runtime.sendMessage(payload);
}

async function getActiveTabUrl() {
  if (!ext.tabs || typeof ext.tabs.query !== "function") {
    return null;
  }
  const tabs = await ext.tabs.query({ active: true, currentWindow: true });
  return tabs && tabs[0] ? tabs[0].url : null;
}

async function refreshState() {
  prefs = Object.assign({}, DEFAULT_PREFS, await ext.storage.local.get(DEFAULT_PREFS));
  lang = resolveLang(prefs.language);
  applyTranslations();
  el.interceptToggle.checked = Boolean(prefs.interceptDownloads);

  const health = await sendMsg({ type: "health" });
  if (!health || !health.ok) {
    el.connPill.textContent = t(lang, "offline");
    el.connPill.className = "pill err";
    el.pairSection.classList.remove("hidden");
    el.actionsSection.classList.add("hidden");
    return;
  }

  const info = health.data;
  if (prefs.token && info.authenticated) {
    el.connPill.textContent = t(lang, "connected");
    el.connPill.className = "pill ok";
    el.pairSection.classList.add("hidden");
    el.actionsSection.classList.remove("hidden");
  } else {
    el.connPill.textContent = t(lang, "unpaired");
    el.connPill.className = "pill warn";
    el.pairSection.classList.remove("hidden");
    el.actionsSection.classList.add("hidden");
  }
}

el.pairBtn.addEventListener("click", async () => {
  if (pollTimer) {
    clearInterval(pollTimer);
    pollTimer = null;
  }
  const res = await sendMsg({ type: "pair", pin: el.pinInput.value });
  if (!res || !res.ok) {
    showFeedback(t(lang, "pairFailed"), true);
    return;
  }
  if (res.data.status === "paired") {
    showFeedback(t(lang, "pairSuccess"), false);
    await refreshState();
    return;
  }
  if (res.data.status === "pending" && res.data.request_id) {
    showFeedback(t(lang, "pairingPending"), false);
    const reqId = res.data.request_id;
    let attempts = 0;
    pollTimer = setInterval(async () => {
      attempts += 1;
      if (attempts > 120) {
        clearInterval(pollTimer);
        pollTimer = null;
        showFeedback(t(lang, "pairFailed"), true);
        return;
      }
      const poll = await sendMsg({ type: "pair-poll", requestId: reqId });
      if (poll && poll.ok && poll.data.status === "paired") {
        clearInterval(pollTimer);
        pollTimer = null;
        showFeedback(t(lang, "pairSuccess"), false);
        await refreshState();
      } else if (poll && poll.ok && poll.data.status === "rejected") {
        clearInterval(pollTimer);
        pollTimer = null;
        showFeedback(t(lang, "pairFailed"), true);
      }
    }, 1000);
  }
});

el.interceptToggle.addEventListener("change", async () => {
  await ext.storage.local.set({ interceptDownloads: Boolean(el.interceptToggle.checked) });
});

el.sendUrlForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const raw = el.sendUrlInput.value.trim();
  if (!raw) {
    return;
  }
  const referer = await getActiveTabUrl();
  const res = await sendMsg({ type: "send-url", url: raw, referer, openInUi: false });
  if (res && res.ok) {
    el.sendUrlInput.value = "";
    showFeedback(t(lang, "actionOk"), false);
  } else {
    showFeedback(t(lang, "actionErr"), true);
  }
});

el.analyzeTabBtn.addEventListener("click", async () => {
  const url = await getActiveTabUrl();
  if (!url) {
    showFeedback(t(lang, "actionErr"), true);
    return;
  }
  const res = await sendMsg({ type: "analyze-tab", url });
  if (res && res.ok) {
    const count = (res.data.candidates || []).length;
    showFeedback(t(lang, "candidatesFound", { n: count }), false);
  } else {
    showFeedback(t(lang, "actionErr"), true);
  }
});

el.cookiesBtn.addEventListener("click", async () => {
  const url = await getActiveTabUrl();
  if (!url) {
    showFeedback(t(lang, "actionErr"), true);
    return;
  }
  const res = await sendMsg({ type: "forward-cookies", url });
  if (res && res.ok) {
    showFeedback(t(lang, "cookiesSent", { n: res.data.stored || 0 }), false);
  } else {
    showFeedback(t(lang, "actionErr"), true);
  }
});

el.altSpeedBtn.addEventListener("click", async () => {
  const res = await sendMsg({ type: "toggle-alt-speed" });
  if (res && res.ok) {
    showFeedback(t(lang, "actionOk"), false);
  } else {
    showFeedback(t(lang, "actionErr"), true);
  }
});

el.disconnectBtn.addEventListener("click", async () => {
  await ext.storage.local.set({ token: "" });
  await refreshState();
});

el.optionsBtn.addEventListener("click", () => {
  if (ext.runtime && typeof ext.runtime.openOptionsPage === "function") {
    ext.runtime.openOptionsPage();
  }
});

el.langBtn.addEventListener("click", async () => {
  const next = lang === "it" ? "en" : "it";
  await ext.storage.local.set({ language: next });
  await refreshState();
});

refreshState();
