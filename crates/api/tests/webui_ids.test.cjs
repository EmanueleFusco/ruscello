const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

class Element {
  children = [];
  listeners = new Map();
  style = {};
  classList = { add() {}, remove() {}, toggle() {} };

  appendChild(child) { this.children.push(child); }
  addEventListener(name, listener) { this.listeners.set(name, listener); }
  click() { return this.listeners.get("click")(); }
  focus() {}
  find(text) {
    if (this.textContent === text) return this;
    return this.children.map((child) => child.find(text)).find(Boolean);
  }
}

function loadClient(fetch, webSocket, language = "en") {
  const pathToClient = path.join(__dirname, "..", "assets", "webui", "app.js");
  const source = fs.readFileSync(pathToClient, "utf8").replace(
    /  boot\(\);\s*\}\)\(\);\s*$/,
    "  Object.assign(globalThis, { boot, renderRow: renderDownloadCard });\n})();\n"
  );
  const elements = new Map();
  const windowEvents = new Map();
  const context = {
    navigator: { language },
    document: {
      documentElement: { setAttribute() {} },
      addEventListener() {},
      getElementById(id) {
        if (!elements.has(id)) elements.set(id, new Element());
        return elements.get(id);
      },
      createElement: () => new Element()
    },
    window: {
      addEventListener(name, handler) { windowEvents.set(name, handler); },
      location: { protocol: "http:", host: "example.test" },
      WebSocket: webSocket
    },
    WebSocket: webSocket,
    fetch
  };
  vm.runInNewContext(source, context);
  return { context, elements, windowEvents };
}

test("row actions use the exact decimal ID after JavaScript parses a live snapshot", async () => {
  const requests = [];
  const { context, elements } = loadClient(async (url, options) => {
    requests.push([String(url), options.method]);
    return { ok: true, json: async () => ({ downloads: [], packages: [] }) };
  });
  const exact = "1925344271502407592";
  const row = JSON.parse(`{"id":${exact},"id_string":"${exact}","status":"Paused"}`);
  assert.notEqual(String(row.id), exact);
  const card = context.renderRow(row);
  await card.find("Resume").click();
  await card.find("Remove").click();
  await elements.get("remove-confirm-btn").click();
  assert.ok(requests.some(([url, method]) => method === "POST" && url.endsWith(`/downloads/${exact}/resume`)));
  assert.ok(requests.some(([url, method]) => method === "DELETE" && url.endsWith(`/downloads/${exact}?delete_files=false`)));
});

test("unsupported torrent binding shows localized recovery guidance", () => {
  for (const [language, guidance] of [
    ["en", "BitTorrent interface binding is unavailable on Windows. Clear the interface field in desktop settings only if you want to use the system network route."],
    ["it", "Il vincolo a un’interfaccia BitTorrent non è disponibile su Windows. Svuota il campo nelle impostazioni dell’app solo se vuoi usare il percorso di rete del sistema."]
  ]) {
    const { context } = loadClient(undefined, undefined, language);
    const row = context.renderRow({ status: "Failed", failure_reason: "TorrentInterfaceUnsupported" });
    assert.ok(row.find(guidance));
  }
});

test("a pre-login focus request cannot stop the stream after a successful sign-in", async () => {
  const anonymous = { auth_required: true, authenticated: false, locale: "en", theme: "system" };
  const queue = { downloads: [], packages: [] };
  const response = (body) => ({ ok: true, json: async () => body });
  let healthCalls = 0;
  let finishHealth;
  let finishQueue;
  const pendingHealth = new Promise((resolve) => { finishHealth = resolve; });
  const pendingQueue = new Promise((resolve) => { finishQueue = resolve; });
  const connections = [];
  class TestSocket {
    constructor(url) { connections.push(url); }
    addEventListener() {}
  }
  const { context, elements, windowEvents } = loadClient(async (url) => {
    if (url === "/api/v1/health") return ++healthCalls === 1 ? response(anonymous) : pendingHealth;
    if (url === "/api/v1/auth/login") return response({ ok: true });
    if (url === "/api/v1/queue") return pendingQueue;
    assert.fail(`Unexpected request to ${url}`);
  }, TestSocket);
  await context.boot();
  windowEvents.get("focus")();
  elements.get("login-password").value = "fixture-password";
  const login = elements.get("login-form").listeners.get("submit")({ preventDefault() {} });
  await new Promise(setImmediate);

  // The focus request was sent without the new cookie and its stale response arrives during login.
  finishHealth(response(anonymous));
  await new Promise(setImmediate);
  finishQueue(response(queue));
  await login;

  assert.deepEqual(connections, ["ws://example.test/api/v1/ws"]);
});
