const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");
const vm = require("node:vm");

const script = fs.readFileSync(path.join(__dirname, "..", "background.js"), "utf8")
  .replaceAll(/^export /gm, "");

const downloadId = "1925344271502407592";
const successfulHandoff = ["pause", "stage", "cancel", "start", "erase"];

function harness({
  changes = {}, onSearch, onPause, onHandoff, onCancel, firefoxProbe, failStart = false,
  response = '{"added_id_strings":["1925344271502407592"]}'
} = {}) {
  let created;
  let now = 0;
  let lookupCount = 0;
  const calls = [];
  let item = {
    id: 7,
    state: "in_progress",
    paused: false,
    bytesReceived: 64 * 1024,
    url: "https://fixture.invalid/payload.bin",
    fileSize: 5 * 1024 * 1024,
    mime: "application/octet-stream",
    ...changes
  };
  const browser = {
    storage: {
      local: { get: async () => ({
        token: "fixture-token",
        interceptDownloads: true,
        minSizeMb: 4,
        mimePrefixes: ["application/octet-stream"]
      }) }
    },
    downloads: {
      onCreated: { addListener: (listener) => { created = listener; } },
      search: async () => {
        if (onSearch) item = onSearch(item, ++lookupCount);
        return item ? [{ ...item }] : [];
      },
      pause: async () => { calls.push("pause"); item = onPause ? onPause(item) : { ...item, paused: true }; },
      resume: async () => { calls.push("resume"); },
      cancel: async () => {
        calls.push("cancel");
        item = onCancel ? onCancel(item) : { ...item, state: "interrupted", paused: false, canResume: false };
      },
      erase: async () => { calls.push("erase"); }
    }
  };
  if (firefoxProbe) browser.runtime = { getBrowserInfo: () => Promise.resolve({ name: "Firefox" }) };
  const context = { browser, URL, Date: { now: () => now }, setTimeout: (fn, delay) => { now += delay; fn(); } };
  context.AbortSignal = AbortSignal;
  context.fetch = firefoxProbe;
  vm.runInNewContext(script, context);
  context.submitDownload = async (_, __, options) => {
    calls.push("stage");
    assert.equal(options.startImmediately, false);
    if (onHandoff) await onHandoff();
    return { text: async () => response };
  };
  context.apiFetch = async (path, options) => {
    const discard = options.method === "DELETE";
    assert.equal(path, "/api/v1/downloads/" + downloadId + (discard ? "?delete_files=false" : "/resume"));
    calls.push(discard ? "discard" : "start");
    return { ok: discard || !failStart, status: 503 };
  };
  return { run: () => created({ ...item }), calls };
}

test("unknown filter metadata leaves the browser download intact", async () => {
  const driver = harness({ changes: { fileSize: -1, totalBytes: -1, mime: "" } });
  await driver.run();
  assert.deepEqual(driver.calls, []);
});

test("observed size below the threshold prevents handoff", async () => {
  const driver = harness({
    changes: { fileSize: -1 },
    onSearch: (item) => ({ ...item, totalBytes: 1024 })
  });
  await driver.run();
  assert.deepEqual(driver.calls, []);
});

test("zero file size does not mask a known positive total", async () => {
  const driver = harness({ changes: { fileSize: 0, totalBytes: 5 * 1024 * 1024 } });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});

test("unavailable MIME metadata keeps a size-eligible download in the browser", async () => {
  const driver = harness({ changes: { mime: null } });
  await driver.run();
  assert.deepEqual(driver.calls, []);
});

test("a later MIME observation can make the download eligible", async () => {
  const driver = harness({
    changes: { mime: "" },
    onSearch: (item) => ({ ...item, mime: "application/octet-stream" })
  });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});

test("a browser download that finishes before pause is not submitted", async () => {
  const driver = harness({ onPause: (item) => ({ ...item, state: "complete" }) });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause"]);
});

test("handoff owns the paused item and erases it after cancellation", async () => {
  const driver = harness({ changes: { fileSize: -1 }, onSearch: (item) => ({ ...item, totalBytes: 5 * 1024 * 1024 }) });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});

test("rejected handoff resumes the native download", async () => {
  const driver = harness({ onHandoff: async () => { throw new Error("fixture rejection"); } });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause", "stage", "resume"]);
});

test("Firefox's interrupted but paused item is eligible for handoff", async () => {
  const driver = harness({
    onPause: (item) => ({ ...item, state: "interrupted", paused: true, canResume: true })
  });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});

test("Firefox resumes its paused interruption after a rejected handoff", async () => {
  const driver = harness({
    onPause: (item) => ({ ...item, state: "interrupted", paused: true, canResume: true }),
    onHandoff: async () => { throw new Error("fixture rejection"); }
  });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause", "stage", "resume"]);
});

test("interrupted state alone does not confirm cancellation", async () => {
  const driver = harness({
    onCancel: (item) => ({ ...item, state: "interrupted", paused: true, canResume: true })
  });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause", "stage", "cancel", "resume", "discard"]);
});

test("interception never pauses a browser item that received no data", async () => {
  const driver = harness({ changes: { bytesReceived: 0 } });
  await driver.run();
  assert.deepEqual(driver.calls, []);
});

test("Firefox skips a source whose range request cannot validate resumption", async () => {
  const driver = harness({
    firefoxProbe: async () => ({ status: 200, headers: new Headers({ "Accept-Ranges": "bytes" }) })
  });
  await driver.run();
  assert.deepEqual(driver.calls, []);
});

test("Firefox confirms a range response before pausing", async () => {
  const driver = harness({
    changes: { bytesReceived: 0 },
    onSearch: (item) => ({ ...item, bytesReceived: 4096 }),
    firefoxProbe: async (_, options) => {
      assert.equal(options.headers.Range, "bytes=0-0");
      return {
        status: 206,
        headers: new Headers({ "Content-Range": "bytes 0-0/5242880", ETag: '"fixture-v1"' })
      };
    },
    onPause: (item) => ({ ...item, state: "interrupted", paused: true, canResume: true })
  });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});

test("Chrome can keep its paused flag after confirmed cancellation", async () => {
  const driver = harness({
    onCancel: (item) => ({ ...item, state: "interrupted", paused: true, canResume: false, error: "USER_CANCELED" })
  });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});

test("a native completion after staging discards the paused desktop job", async () => {
  const driver = harness({
    onCancel: (item) => ({ ...item, state: "complete", paused: true, canResume: false })
  });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause", "stage", "cancel", "discard"]);
});

test("an ambiguous native cancellation leaves the paused desktop job recoverable", async () => {
  const driver = harness({ onCancel: () => undefined });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause", "stage", "cancel"]);
});

test("a failed desktop activation keeps the staged job and the native history entry", async () => {
  const driver = harness({ failStart: true });
  await driver.run();
  assert.deepEqual(driver.calls, ["pause", "stage", "cancel", "start"]);
});

test("the staged job keeps the exact ID returned by an older desktop", async () => {
  const driver = harness({ response: '{"added_ids":[1925344271502407592]}' });
  await driver.run();
  assert.deepEqual(driver.calls, successfulHandoff);
});
