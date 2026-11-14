import { resolveLang, t } from "./i18n.js";

const ext = globalThis.browser ?? globalThis.chrome;

const DEFAULT_PREFS = {
  apiPort: 9667,
  minSizeMb: 0,
  mimePrefixes: ["application/", "video/", "audio/"],
  language: "auto"
};

const el = {
  optionsTitle: document.getElementById("options-title"),
  apiPortLabel: document.getElementById("api-port-label"),
  apiPort: document.getElementById("api-port"),
  minSizeLabel: document.getElementById("min-size-label"),
  minSize: document.getElementById("min-size"),
  mimeLabel: document.getElementById("mime-label"),
  mimePrefixes: document.getElementById("mime-prefixes"),
  languageLabel: document.getElementById("language-label"),
  languageSelect: document.getElementById("language-select"),
  saveBtn: document.getElementById("save-btn"),
  savedMsg: document.getElementById("saved-msg"),
  form: document.getElementById("options-form")
};

function applyLang(lang) {
  el.optionsTitle.textContent = t(lang, "optionsTitle");
  el.apiPortLabel.textContent = t(lang, "apiPortLabel");
  el.minSizeLabel.textContent = t(lang, "minSizeLabel");
  el.mimeLabel.textContent = t(lang, "mimeLabel");
  el.languageLabel.textContent = t(lang, "languageLabel");
  el.saveBtn.textContent = t(lang, "saveBtn");
}

async function init() {
  const stored = Object.assign({}, DEFAULT_PREFS, await ext.storage.local.get(DEFAULT_PREFS));
  const lang = resolveLang(stored.language);
  applyLang(lang);
  el.apiPort.value = stored.apiPort;
  el.minSize.value = stored.minSizeMb;
  el.mimePrefixes.value = Array.isArray(stored.mimePrefixes)
    ? stored.mimePrefixes.join(", ")
    : "";
  el.languageSelect.value = stored.language || "auto";
}

el.form.addEventListener("submit", async (event) => {
  event.preventDefault();
  const apiPort = Math.max(1, Math.min(65535, Number(el.apiPort.value) || 9667));
  const minSizeMb = Math.max(0, Number(el.minSize.value) || 0);
  const mimePrefixes = el.mimePrefixes.value
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
  const language = el.languageSelect.value;

  await ext.storage.local.set({ apiPort, minSizeMb, mimePrefixes, language });
  const lang = resolveLang(language);
  applyLang(lang);
  el.savedMsg.textContent = t(lang, "savedMsg");
  el.savedMsg.classList.remove("hidden");
});

init();
