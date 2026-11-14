(function () {
  "use strict";

  const STRINGS = {
    en: {
      appTitle: "Ruscello",
      loginSubtitle: "Enter password to control remote downloads",
      passwordLabel: "Password or PIN",
      show: "Show",
      hide: "Hide",
      signIn: "Sign in",
      signOut: "Sign out",
      invalidCredentials: "Invalid password.",
      rateLimited: "Too many attempts. Retry in {s}s.",
      live: "Live",
      reconnecting: "Reconnecting…",
      offline: "Offline",
      altSpeed: "Alt speed",
      pauseAll: "Pause all",
      resumeAll: "Resume all",
      add: "+ Add",
      filterAll: "All",
      filterActive: "Active",
      filterQueued: "Queued",
      filterPaused: "Paused",
      filterCompleted: "Completed",
      filterFailed: "Failed",
      searchPlaceholder: "Search downloads or packages…",
      emptyTitle: "No downloads to show",
      emptyHint: "Tap + Add to queue URLs, magnets, .torrent or .dlc files.",
      addTitle: "Add downloads",
      urlsLabel: "URLs or magnet links (one per line)",
      packageLabel: "Package name (optional)",
      fileLabel: ".torrent or .dlc container",
      startNow: "Start immediately",
      cancel: "Cancel",
      submitAdd: "Queue downloads",
      addEmptyError: "Provide at least one URL or container file.",
      addDone: "Downloads added to queue.",
      pause: "Pause",
      resume: "Resume",
      forceStart: "Force start",
      remove: "Remove",
      removeTitle: "Remove download",
      deleteFiles: "Also delete downloaded files from disk",
      confirmRemove: "Remove",
      statusQueued: "Queued",
      statusResolving: "Resolving",
      statusWaitingUser: "Verification needed",
      statusDownloading: "Downloading",
      statusPaused: "Paused",
      statusSeeding: "Seeding",
      statusPostProcessing: "Processing",
      statusCompleted: "Completed",
      statusFailed: "Failed",
      actionFailed: "Action could not be completed.",
      itemsCount: "{n} items",
      unknownSize: "Unknown size",
      etaPrefix: "ETA",
      langLabel: "IT"
    },
    it: {
      appTitle: "Ruscello",
      loginSubtitle: "Inserisci la password per gestire i download in remoto",
      passwordLabel: "Password o PIN",
      show: "Mostra",
      hide: "Nascondi",
      signIn: "Accedi",
      signOut: "Esci",
      invalidCredentials: "Password non valida.",
      rateLimited: "Troppi tentativi. Riprova tra {s}s.",
      live: "In diretta",
      reconnecting: "Riconnessione…",
      offline: "Disconnesso",
      altSpeed: "Velocità alt.",
      pauseAll: "Metti in pausa tutto",
      resumeAll: "Riprendi tutto",
      add: "+ Aggiungi",
      filterAll: "Tutti",
      filterActive: "Attivi",
      filterQueued: "In coda",
      filterPaused: "In pausa",
      filterCompleted: "Completati",
      filterFailed: "Non riusciti",
      searchPlaceholder: "Cerca download o pacchetti…",
      emptyTitle: "Nessun download da mostrare",
      emptyHint: "Tocca + Aggiungi per accodare URL, magnet, file .torrent o .dlc.",
      addTitle: "Aggiungi download",
      urlsLabel: "URL o link magnet (uno per riga)",
      packageLabel: "Nome pacchetto (opzionale)",
      fileLabel: "File .torrent o .dlc",
      startNow: "Avvia subito",
      cancel: "Annulla",
      submitAdd: "Metti in coda",
      addEmptyError: "Inserisci almeno un URL o un file container.",
      addDone: "Download aggiunti alla coda.",
      pause: "Pausa",
      resume: "Riprendi",
      forceStart: "Avvio forzato",
      remove: "Rimuovi",
      removeTitle: "Rimuovi download",
      deleteFiles: "Elimina anche i file scaricati dal disco",
      confirmRemove: "Rimuovi",
      statusQueued: "In coda",
      statusResolving: "Risoluzione",
      statusWaitingUser: "Verifica richiesta",
      statusDownloading: "In scaricamento",
      statusPaused: "In pausa",
      statusSeeding: "Condivisione",
      statusPostProcessing: "Elaborazione",
      statusCompleted: "Completato",
      statusFailed: "Non riuscito",
      actionFailed: "Impossibile completare l'operazione.",
      itemsCount: "{n} elementi",
      unknownSize: "Dimensione sconosciuta",
      etaPrefix: "ETA",
      langLabel: "EN"
    }
  };

  const state = {
    lang: detectInitialLang(),
    authRequired: false,
    authenticated: false,
    filter: "all",
    search: "",
    snapshot: null,
    collapsedPackages: new Set(),
    removeTarget: null,
    selectedFile: null,
    ws: null,
    sse: null,
    reconnectTimer: null,
    toastTimer: null,
    connectionMode: "offline"
  };

  const el = {
    loginView: document.getElementById("login-view"),
    mainView: document.getElementById("main-view"),
    loginSubtitle: document.getElementById("login-subtitle"),
    loginPasswordLabel: document.getElementById("login-password-label"),
    loginPassword: document.getElementById("login-password"),
    togglePasswordBtn: document.getElementById("toggle-password-btn"),
    loginError: document.getElementById("login-error"),
    loginSubmitBtn: document.getElementById("login-submit-btn"),
    loginLangBtn: document.getElementById("login-lang-btn"),
    loginForm: document.getElementById("login-form"),
    appTitle: document.getElementById("app-title"),
    connBadge: document.getElementById("conn-badge"),
    statDown: document.getElementById("stat-down"),
    statUp: document.getElementById("stat-up"),
    statCounts: document.getElementById("stat-counts"),
    altSpeedBtn: document.getElementById("alt-speed-btn"),
    altSpeedLabel: document.getElementById("alt-speed-label"),
    pauseAllBtn: document.getElementById("pause-all-btn"),
    addOpenBtn: document.getElementById("add-open-btn"),
    langBtn: document.getElementById("lang-btn"),
    logoutBtn: document.getElementById("logout-btn"),
    filterTabs: document.getElementById("filter-tabs"),
    searchInput: document.getElementById("search-input"),
    toastBanner: document.getElementById("toast-banner"),
    emptyState: document.getElementById("empty-state"),
    emptyTitle: document.getElementById("empty-title"),
    emptyHint: document.getElementById("empty-hint"),
    queueList: document.getElementById("queue-list"),
    addModal: document.getElementById("add-modal"),
    addModalTitle: document.getElementById("add-modal-title"),
    addCloseBtn: document.getElementById("add-close-btn"),
    addForm: document.getElementById("add-form"),
    addUrlsLabel: document.getElementById("add-urls-label"),
    addUrls: document.getElementById("add-urls"),
    addPackageLabel: document.getElementById("add-package-label"),
    addPackage: document.getElementById("add-package"),
    addFileLabel: document.getElementById("add-file-label"),
    addFile: document.getElementById("add-file"),
    addFileName: document.getElementById("add-file-name"),
    addStartNow: document.getElementById("add-start-now"),
    addStartLabel: document.getElementById("add-start-label"),
    addError: document.getElementById("add-error"),
    addCancelBtn: document.getElementById("add-cancel-btn"),
    addSubmitBtn: document.getElementById("add-submit-btn"),
    removeModal: document.getElementById("remove-modal"),
    removeModalTitle: document.getElementById("remove-modal-title"),
    removeCloseBtn: document.getElementById("remove-close-btn"),
    removeTargetName: document.getElementById("remove-target-name"),
    removeDeleteFiles: document.getElementById("remove-delete-files"),
    removeDeleteLabel: document.getElementById("remove-delete-label"),
    removeCancelBtn: document.getElementById("remove-cancel-btn"),
    removeConfirmBtn: document.getElementById("remove-confirm-btn")
  };

  function detectInitialLang() {
    try {
      const saved = window.localStorage.getItem("ruscello_lang");
      if (saved === "en" || saved === "it") {
        return saved;
      }
    } catch (_) {}
    return (navigator.language || "").toLowerCase().startsWith("it") ? "it" : "en";
  }

  function t(key, vars) {
    const dict = STRINGS[state.lang] || STRINGS.en;
    let out = dict[key] || STRINGS.en[key] || key;
    if (vars) {
      for (const [k, v] of Object.entries(vars)) {
        out = out.replace("{" + k + "}", String(v));
      }
    }
    return out;
  }

  function setLang(lang) {
    state.lang = lang === "it" ? "it" : "en";
    document.documentElement.lang = state.lang;
    try {
      window.localStorage.setItem("ruscello_lang", state.lang);
    } catch (_) {}
    applyTranslations();
    renderQueue();
  }

  function applyTranslations() {
    el.loginSubtitle.textContent = t("loginSubtitle");
    el.loginPasswordLabel.textContent = t("passwordLabel");
    el.togglePasswordBtn.textContent = el.loginPassword.type === "password" ? t("show") : t("hide");
    el.loginSubmitBtn.textContent = t("signIn");
    el.loginLangBtn.textContent = t("langLabel");
    el.appTitle.textContent = t("appTitle");
    el.altSpeedLabel.textContent = t("altSpeed");
    el.addOpenBtn.textContent = t("add");
    el.langBtn.textContent = t("langLabel");
    el.logoutBtn.textContent = t("signOut");
    el.searchInput.placeholder = t("searchPlaceholder");
    el.emptyTitle.textContent = t("emptyTitle");
    el.emptyHint.textContent = t("emptyHint");
    el.addModalTitle.textContent = t("addTitle");
    el.addUrlsLabel.textContent = t("urlsLabel");
    el.addPackageLabel.textContent = t("packageLabel");
    el.addFileLabel.textContent = t("fileLabel");
    el.addStartLabel.textContent = t("startNow");
    el.addCancelBtn.textContent = t("cancel");
    el.addSubmitBtn.textContent = t("submitAdd");
    el.removeModalTitle.textContent = t("removeTitle");
    el.removeDeleteLabel.textContent = t("deleteFiles");
    el.removeCancelBtn.textContent = t("cancel");
    el.removeConfirmBtn.textContent = t("confirmRemove");
    updateConnectionBadge();
    renderFilterTabs();
    updatePauseAllButton();
  }

  function updateConnectionBadge() {
    el.connBadge.classList.remove("live", "offline");
    if (state.connectionMode === "live") {
      el.connBadge.textContent = t("live");
      el.connBadge.classList.add("live");
    } else if (state.connectionMode === "reconnecting") {
      el.connBadge.textContent = t("reconnecting");
      el.connBadge.classList.add("offline");
    } else {
      el.connBadge.textContent = t("offline");
      el.connBadge.classList.add("offline");
    }
  }

  function renderFilterTabs() {
    const defs = [
      ["all", t("filterAll")],
      ["active", t("filterActive")],
      ["queued", t("filterQueued")],
      ["paused", t("filterPaused")],
      ["completed", t("filterCompleted")],
      ["failed", t("filterFailed")]
    ];
    el.filterTabs.textContent = "";
    for (const [id, label] of defs) {
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = "filter-pill" + (state.filter === id ? " active" : "");
      btn.textContent = label;
      btn.addEventListener("click", () => {
        state.filter = id;
        renderFilterTabs();
        renderQueue();
      });
      el.filterTabs.appendChild(btn);
    }
  }

  function formatBytes(bytes) {
    if (typeof bytes !== "number" || !Number.isFinite(bytes) || bytes < 0) {
      return "0 B";
    }
    const units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let val = bytes;
    let idx = 0;
    while (val >= 1024 && idx < units.length - 1) {
      val /= 1024;
      idx++;
    }
    return (idx === 0 ? Math.round(val) : val.toFixed(val >= 10 ? 1 : 2)) + " " + units[idx];
  }

  function formatSpeed(bps) {
    return formatBytes(bps || 0) + "/s";
  }

  function formatEta(secs) {
    if (typeof secs !== "number" || !Number.isFinite(secs) || secs <= 0) {
      return "--";
    }
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = Math.floor(secs % 60);
    const pad = (n) => String(n).padStart(2, "0");
    return h > 0 ? h + ":" + pad(m) + ":" + pad(s) : pad(m) + ":" + pad(s);
  }

  function showToast(msg) {
    if (state.toastTimer) {
      clearTimeout(state.toastTimer);
    }
    el.toastBanner.textContent = msg;
    el.toastBanner.classList.remove("hidden");
    state.toastTimer = setTimeout(() => {
      el.toastBanner.classList.add("hidden");
    }, 3200);
  }

  async function apiFetch(path, options) {
    const resp = await fetch(path, Object.assign({ credentials: "same-origin" }, options || {}));
    if (resp.status === 401) {
      state.authenticated = false;
      state.authRequired = true;
      closeStreams();
      showView();
      throw new Error("unauthorized");
    }
    return resp;
  }

  async function boot() {
    applyTranslations();
    try {
      const resp = await fetch("/api/v1/health", { credentials: "same-origin" });
      if (resp.ok) {
        const health = await resp.json();
        state.authRequired = Boolean(health.auth_required);
        state.authenticated = Boolean(health.authenticated) || !state.authRequired;
        try {
          if (!window.localStorage.getItem("ruscello_lang") && (health.locale === "en" || health.locale === "it")) {
            setLang(health.locale);
          }
        } catch (_) {}
      }
    } catch (_) {}
    showView();
    if (state.authenticated) {
      await refreshSnapshot();
      connectStream();
    }
  }

  function showView() {
    if (state.authRequired && !state.authenticated) {
      el.loginView.classList.remove("hidden");
      el.mainView.classList.add("hidden");
      el.logoutBtn.classList.add("hidden");
      el.loginPassword.focus();
    } else {
      el.loginView.classList.add("hidden");
      el.mainView.classList.remove("hidden");
      el.logoutBtn.classList.toggle("hidden", !state.authRequired);
    }
  }

  async function refreshSnapshot() {
    try {
      const resp = await apiFetch("/api/v1/queue");
      if (resp.ok) {
        state.snapshot = await resp.json();
        renderHeaderStats();
        renderQueue();
      }
    } catch (_) {}
  }

  function closeStreams() {
    if (state.reconnectTimer) {
      clearTimeout(state.reconnectTimer);
      state.reconnectTimer = null;
    }
    if (state.ws) {
      try { state.ws.close(); } catch (_) {}
      state.ws = null;
    }
    if (state.sse) {
      try { state.sse.close(); } catch (_) {}
      state.sse = null;
    }
    state.connectionMode = "offline";
    updateConnectionBadge();
  }

  function scheduleReconnect() {
    if (!state.authenticated || state.reconnectTimer) {
      return;
    }
    state.connectionMode = "reconnecting";
    updateConnectionBadge();
    state.reconnectTimer = setTimeout(() => {
      state.reconnectTimer = null;
      connectStream();
    }, 2500);
  }

  function handleStreamMessage(payload) {
    if (!payload || typeof payload !== "object") {
      return;
    }
    if (payload.type === "snapshot" && payload.data) {
      state.snapshot = payload.data;
      renderHeaderStats();
      renderQueue();
    }
  }

  function connectStream() {
    closeStreams();
    if (!state.authenticated) {
      return;
    }
    if ("WebSocket" in window) {
      const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
      const wsUrl = proto + "//" + window.location.host + "/api/v1/ws";
      try {
        const ws = new WebSocket(wsUrl);
        state.ws = ws;
        ws.addEventListener("open", () => {
          state.connectionMode = "live";
          updateConnectionBadge();
        });
        ws.addEventListener("message", (event) => {
          try {
            handleStreamMessage(JSON.parse(event.data));
          } catch (_) {}
        });
        ws.addEventListener("error", () => {
          try { ws.close(); } catch (_) {}
        });
        ws.addEventListener("close", () => {
          if (state.ws === ws) {
            state.ws = null;
            connectSseFallback();
          }
        });
        return;
      } catch (_) {}
    }
    connectSseFallback();
  }

  function connectSseFallback() {
    if (!state.authenticated || !("EventSource" in window)) {
      scheduleReconnect();
      return;
    }
    try {
      const sse = new EventSource("/api/v1/events", { withCredentials: true });
      state.sse = sse;
      sse.addEventListener("open", () => {
        state.connectionMode = "live";
        updateConnectionBadge();
      });
      sse.addEventListener("snapshot", (event) => {
        try {
          handleStreamMessage({ type: "snapshot", data: JSON.parse(event.data) });
        } catch (_) {}
      });
      sse.addEventListener("error", () => {
        if (state.sse === sse) {
          sse.close();
          state.sse = null;
          scheduleReconnect();
        }
      });
    } catch (_) {
      scheduleReconnect();
    }
  }

  function renderHeaderStats() {
    const snap = state.snapshot;
    if (!snap) {
      return;
    }
    el.statDown.textContent = "↓ " + formatSpeed(snap.total_download_bps || 0);
    el.statUp.textContent = "↑ " + formatSpeed(snap.total_upload_bps || 0);
    el.statCounts.textContent = t("itemsCount", { n: (snap.downloads || []).length });
    el.altSpeedBtn.classList.toggle("active", Boolean(snap.alternate_speed_enabled));
    updatePauseAllButton();
  }

  function canPauseDownload(d) {
    return ["Queued", "Resolving", "WaitingUser", "Downloading", "Seeding"].includes(d.status);
  }

  function canResumeDownload(d) {
    return d.status === "Paused" || d.status === "Failed";
  }

  function canPauseAll() {
    const downloads = (state.snapshot && state.snapshot.downloads) || [];
    return downloads.some(canPauseDownload);
  }

  function updatePauseAllButton() {
    const downloads = (state.snapshot && state.snapshot.downloads) || [];
    const pause = canPauseAll();
    const resume = downloads.some((d) => d.status === "Paused");
    el.pauseAllBtn.textContent = pause ? t("pauseAll") : t("resumeAll");
    el.pauseAllBtn.disabled = !pause && !resume;
  }

  function matchesFilter(d) {
    switch (state.filter) {
      case "active":
        return ["Downloading", "Resolving", "PostProcessing", "Seeding", "WaitingUser"].includes(d.status);
      case "queued":
        return d.status === "Queued";
      case "paused":
        return d.status === "Paused";
      case "completed":
        return d.status === "Completed";
      case "failed":
        return d.status === "Failed";
      default:
        return true;
    }
  }

  function matchesSearch(d, pkgName) {
    const q = state.search.trim().toLowerCase();
    if (!q) {
      return true;
    }
    return (
      (d.filename || "").toLowerCase().includes(q) ||
      (d.source_summary || "").toLowerCase().includes(q) ||
      (pkgName || "").toLowerCase().includes(q)
    );
  }

  function statusLabel(status) {
    return t("status" + status);
  }

  function renderDownloadCard(d) {
    const card = document.createElement("article");
    card.className = "dl-card";

    const top = document.createElement("div");
    top.className = "dl-top";

    const titleWrap = document.createElement("div");
    const title = document.createElement("div");
    title.className = "dl-title";
    title.textContent = d.filename || d.source_summary || ("#" + d.id);
    titleWrap.appendChild(title);

    const badge = document.createElement("span");
    badge.className = "status-pill status-" + d.status;
    badge.textContent = statusLabel(d.status);

    top.appendChild(titleWrap);
    top.appendChild(badge);
    card.appendChild(top);

    const progressTrack = document.createElement("div");
    progressTrack.className = "progress-track";
    const fill = document.createElement("div");
    fill.className = "progress-fill";
    const pct = typeof d.progress_percent === "number" ? Math.max(0, Math.min(100, d.progress_percent)) : 0;
    fill.style.width = pct.toFixed(1) + "%";
    progressTrack.appendChild(fill);
    card.appendChild(progressTrack);

    const meta = document.createElement("div");
    meta.className = "dl-meta";
    const sizeText = d.total_bytes != null
      ? formatBytes(d.downloaded_bytes || 0) + " / " + formatBytes(d.total_bytes)
      : formatBytes(d.downloaded_bytes || 0) + " (" + t("unknownSize") + ")";
    const parts = [pct.toFixed(1) + "%", sizeText];
    if (d.download_bps > 0) {
      parts.push("↓ " + formatSpeed(d.download_bps));
    }
    if (d.upload_bps > 0) {
      parts.push("↑ " + formatSpeed(d.upload_bps));
    }
    if (d.eta_secs != null && d.status === "Downloading") {
      parts.push(t("etaPrefix") + " " + formatEta(d.eta_secs));
    }
    if (d.failure_reason) {
      parts.push(d.failure_reason);
    }
    for (const part of parts) {
      const span = document.createElement("span");
      span.textContent = part;
      meta.appendChild(span);
    }
    card.appendChild(meta);

    const actions = document.createElement("div");
    actions.className = "dl-actions";
    if (canPauseDownload(d)) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "btn btn-sm";
      b.textContent = t("pause");
      b.addEventListener("click", () => postAction("/api/v1/downloads/" + d.id + "/pause"));
      actions.appendChild(b);
    }
    if (canResumeDownload(d)) {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "btn btn-primary btn-sm";
      b.textContent = t("resume");
      b.addEventListener("click", () => postAction("/api/v1/downloads/" + d.id + "/resume"));
      actions.appendChild(b);
    }
    if (d.status === "Queued" || d.status === "Paused") {
      const b = document.createElement("button");
      b.type = "button";
      b.className = "btn btn-sm";
      b.textContent = t("forceStart");
      b.addEventListener("click", () => postAction("/api/v1/downloads/" + d.id + "/force-start"));
      actions.appendChild(b);
    }
    const rm = document.createElement("button");
    rm.type = "button";
    rm.className = "btn btn-danger btn-sm";
    rm.textContent = t("remove");
    rm.addEventListener("click", () => openRemoveModal(d));
    actions.appendChild(rm);

    card.appendChild(actions);
    return card;
  }

  function renderQueue() {
    el.queueList.textContent = "";
    const snap = state.snapshot;
    if (!snap || !Array.isArray(snap.downloads)) {
      el.emptyState.classList.remove("hidden");
      return;
    }

    const pkgById = new Map();
    for (const pkg of snap.packages || []) {
      pkgById.set(pkg.id, pkg);
    }

    const grouped = new Map();
    const standalone = [];
    for (const d of snap.downloads) {
      const pkg = d.package_id != null ? pkgById.get(d.package_id) : null;
      const pkgName = pkg ? pkg.name : "";
      if (!matchesFilter(d) || !matchesSearch(d, pkgName)) {
        continue;
      }
      if (pkg) {
        if (!grouped.has(pkg.id)) {
          grouped.set(pkg.id, { pkg, items: [] });
        }
        grouped.get(pkg.id).items.push(d);
      } else {
        standalone.push(d);
      }
    }

    const totalVisible = standalone.length + Array.from(grouped.values()).reduce((a, g) => a + g.items.length, 0);
    el.emptyState.classList.toggle("hidden", totalVisible > 0);

    for (const { pkg, items } of grouped.values()) {
      const wrap = document.createElement("section");
      wrap.className = "package-group";

      const collapsed = state.collapsedPackages.has(pkg.id);
      const header = document.createElement("button");
      header.type = "button";
      header.className = "package-header";
      header.addEventListener("click", () => {
        if (state.collapsedPackages.has(pkg.id)) {
          state.collapsedPackages.delete(pkg.id);
        } else {
          state.collapsedPackages.add(pkg.id);
        }
        renderQueue();
      });

      const left = document.createElement("strong");
      left.textContent = (collapsed ? "▸ " : "▾ ") + pkg.name;
      const right = document.createElement("span");
      right.className = "muted small-note";
      right.textContent = t("itemsCount", { n: items.length });
      header.appendChild(left);
      header.appendChild(right);
      wrap.appendChild(header);

      if (!collapsed) {
        const list = document.createElement("div");
        list.className = "package-items";
        for (const item of items) {
          list.appendChild(renderDownloadCard(item));
        }
        wrap.appendChild(list);
      }
      el.queueList.appendChild(wrap);
    }

    for (const item of standalone) {
      el.queueList.appendChild(renderDownloadCard(item));
    }
  }

  async function postAction(path, body) {
    try {
      const resp = await apiFetch(path, {
        method: "POST",
        headers: body ? { "Content-Type": "application/json" } : undefined,
        body: body ? JSON.stringify(body) : undefined
      });
      if (!resp.ok) {
        showToast(t("actionFailed"));
        return false;
      }
      await refreshSnapshot();
      return true;
    } catch (_) {
      return false;
    }
  }

  function openRemoveModal(d) {
    state.removeTarget = d;
    el.removeTargetName.textContent = d.filename || d.source_summary || ("#" + d.id);
    el.removeDeleteFiles.checked = false;
    el.removeModal.classList.remove("hidden");
  }

  function closeRemoveModal() {
    state.removeTarget = null;
    el.removeModal.classList.add("hidden");
  }

  function openAddModal() {
    el.addError.classList.add("hidden");
    el.addUrls.value = "";
    el.addPackage.value = "";
    el.addFile.value = "";
    el.addFileName.textContent = "";
    state.selectedFile = null;
    el.addStartNow.checked = true;
    el.addModal.classList.remove("hidden");
    el.addUrls.focus();
  }

  function closeAddModal() {
    el.addModal.classList.add("hidden");
  }

  function readFileAsBase64(file) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => {
        const bytes = new Uint8Array(reader.result);
        let binary = "";
        for (let i = 0; i < bytes.length; i += 0x8000) {
          binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
        }
        resolve(btoa(binary));
      };
      reader.onerror = () => reject(reader.error);
      reader.readAsArrayBuffer(file);
    });
  }

  el.togglePasswordBtn.addEventListener("click", () => {
    el.loginPassword.type = el.loginPassword.type === "password" ? "text" : "password";
    el.togglePasswordBtn.textContent = el.loginPassword.type === "password" ? t("show") : t("hide");
  });

  el.loginLangBtn.addEventListener("click", () => setLang(state.lang === "en" ? "it" : "en"));
  el.langBtn.addEventListener("click", () => setLang(state.lang === "en" ? "it" : "en"));

  el.loginForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    el.loginError.classList.add("hidden");
    const password = el.loginPassword.value;
    try {
      const resp = await fetch("/api/v1/auth/login", {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ password })
      });
      if (resp.status === 429) {
        const retry = resp.headers.get("Retry-After") || "60";
        el.loginError.textContent = t("rateLimited", { s: retry });
        el.loginError.classList.remove("hidden");
        return;
      }
      if (!resp.ok) {
        el.loginError.textContent = t("invalidCredentials");
        el.loginError.classList.remove("hidden");
        return;
      }
      el.loginPassword.value = "";
      state.authenticated = true;
      showView();
      await refreshSnapshot();
      connectStream();
    } catch (_) {
      el.loginError.textContent = t("actionFailed");
      el.loginError.classList.remove("hidden");
    }
  });

  el.logoutBtn.addEventListener("click", async () => {
    try {
      await fetch("/api/v1/auth/logout", { method: "POST", credentials: "same-origin" });
    } catch (_) {}
    state.authenticated = false;
    state.snapshot = null;
    closeStreams();
    showView();
  });

  el.altSpeedBtn.addEventListener("click", () => {
    const enabled = !(state.snapshot && state.snapshot.alternate_speed_enabled);
    postAction("/api/v1/speed/alternate", { enabled });
  });

  el.pauseAllBtn.addEventListener("click", () => {
    postAction(canPauseAll() ? "/api/v1/downloads/pause-all" : "/api/v1/downloads/resume-all");
  });

  el.searchInput.addEventListener("input", () => {
    state.search = el.searchInput.value;
    renderQueue();
  });

  el.addOpenBtn.addEventListener("click", openAddModal);
  el.addCloseBtn.addEventListener("click", closeAddModal);
  el.addCancelBtn.addEventListener("click", closeAddModal);
  el.addFile.addEventListener("change", () => {
    const file = el.addFile.files && el.addFile.files[0];
    state.selectedFile = file || null;
    el.addFileName.textContent = file ? file.name : "";
  });

  el.addForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    el.addError.classList.add("hidden");
    const urls = el.addUrls.value
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter(Boolean);
    const packageName = el.addPackage.value.trim() || null;
    const startImmediately = Boolean(el.addStartNow.checked);
    const file = state.selectedFile;

    if (urls.length === 0 && !file) {
      el.addError.textContent = t("addEmptyError");
      el.addError.classList.remove("hidden");
      return;
    }

    const payload = {
      urls,
      package_name: packageName,
      start_immediately: startImmediately
    };

    try {
      if (file) {
        const lower = file.name.toLowerCase();
        if (lower.endsWith(".torrent")) {
          payload.torrent_base64 = await readFileAsBase64(file);
          payload.torrent_filename = file.name;
        } else if (lower.endsWith(".dlc")) {
          payload.dlc_content = await file.text();
          payload.dlc_filename = file.name;
        } else {
          const text = await file.text();
          for (const line of text.split(/\r?\n/)) {
            const trimmed = line.trim();
            if (trimmed && !trimmed.startsWith("#")) {
              payload.urls.push(trimmed);
            }
          }
        }
      }
      const resp = await apiFetch("/api/v1/downloads", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(payload)
      });
      if (!resp.ok) {
        const err = await resp.json().catch(() => null);
        el.addError.textContent = (err && err.error) || t("actionFailed");
        el.addError.classList.remove("hidden");
        return;
      }
      closeAddModal();
      showToast(t("addDone"));
      await refreshSnapshot();
    } catch (_) {
      el.addError.textContent = t("actionFailed");
      el.addError.classList.remove("hidden");
    }
  });

  el.removeCloseBtn.addEventListener("click", closeRemoveModal);
  el.removeCancelBtn.addEventListener("click", closeRemoveModal);
  el.removeConfirmBtn.addEventListener("click", async () => {
    if (!state.removeTarget) {
      return;
    }
    const id = state.removeTarget.id;
    const deleteFiles = Boolean(el.removeDeleteFiles.checked);
    closeRemoveModal();
    try {
      const resp = await apiFetch("/api/v1/downloads/" + id + "?delete_files=" + deleteFiles, {
        method: "DELETE"
      });
      if (!resp.ok) {
        showToast(t("actionFailed"));
        return;
      }
      await refreshSnapshot();
    } catch (_) {}
  });

  boot();
})();
