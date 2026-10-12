import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { launchChrome } from "./chrome-launcher.mjs";

async function fakeChrome(t, body) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ruwasm-fake-chrome-"));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  const executable = path.join(dir, "chrome");
  fs.writeFileSync(executable, `#!${process.execPath}\n${body}`, { mode: 0o755 });
  return { executable, dir };
}

test("waits for delayed readiness without a stderr announcement", async (t) => {
  const { executable } = await fakeChrome(t, `
    const fs = require("node:fs");
    const dir = process.argv.find(arg => arg.startsWith("--user-data-dir=")).split("=")[1];
    setTimeout(() => fs.writeFileSync(dir + "/DevToolsActivePort", "12345\\n/devtools/browser/test\\n"), 150);
    setInterval(() => {}, 1000);
  `);
  const browser = await launchChrome(executable, { startupTimeoutMs: 2_000 });
  assert.equal(browser.wsUrl, "ws://127.0.0.1:12345/devtools/browser/test");
  await browser.close();
  assert.notEqual(browser.chrome.signalCode, null);
  assert.equal(fs.existsSync(browser.userDataDir), false);
});

test("accepts the stderr announcement", async (t) => {
  const { executable } = await fakeChrome(t, `
    process.stderr.write("DevTools listening on ws://127.0.0.1:12345/devtools/browser/test\\n");
    setInterval(() => {}, 1000);
  `);
  const browser = await launchChrome(executable, { startupTimeoutMs: 2_000 });
  assert.equal(browser.wsUrl, "ws://127.0.0.1:12345/devtools/browser/test");
  await browser.close();
});

test("kills Chrome and removes its profile when startup times out", async (t) => {
  const { executable, dir } = await fakeChrome(t, "");
  // Record the child's PID and profile outside its profile so cleanup is observable.
  fs.writeFileSync(executable, `#!${process.execPath}\n
    const fs = require("node:fs");
    const profile = process.argv.find(arg => arg.startsWith("--user-data-dir=")).split("=")[1];
    fs.writeFileSync(${JSON.stringify(path.join(dir, "state"))}, JSON.stringify({ pid: process.pid, profile }));
    setInterval(() => {}, 1000);
  `);
  await assert.rejects(launchChrome(executable, { startupTimeoutMs: 500 }), /timed out waiting for Chrome DevTools URL/);
  const state = JSON.parse(fs.readFileSync(path.join(dir, "state"), "utf8"));
  assert.equal(fs.existsSync(state.profile), false);
  assert.throws(() => process.kill(state.pid, 0), { code: "ESRCH" });
});

test("reports an early exit with Chrome diagnostics", async (t) => {
  const { executable } = await fakeChrome(t, `
    process.stderr.write("startup failed\\n");
    process.exit(7);
  `);
  await assert.rejects(launchChrome(executable), /Chrome exited before DevTools was ready: 7\nstartup failed/);
});

test("reports a missing executable", async () => {
  await assert.rejects(launchChrome("/nonexistent/ruwasm-chrome"), /could not launch Chrome:.*ENOENT/);
});
