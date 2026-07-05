#!/usr/bin/env node
import crypto from "node:crypto";
import fs from "node:fs";
import http from "node:http";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { spawn, spawnSync } from "node:child_process";

const siteRoot = process.argv[2];
if (!siteRoot) {
  console.error("usage: tests/browser-smoke.mjs <site-root>");
  process.exit(2);
}

const resolvedSiteRoot = path.resolve(siteRoot);
if (!fs.existsSync(path.join(resolvedSiteRoot, "index.html"))) {
  console.error(`missing index.html in ${resolvedSiteRoot}`);
  process.exit(2);
}

const CONTENT_TYPES = {
  ".html": "text/html",
  ".js": "text/javascript",
  ".css": "text/css",
  ".wasm": "application/wasm",
};

function contentType(file) {
  return CONTENT_TYPES[path.extname(file)] || "application/octet-stream";
}

async function startStaticServer(root) {
  const server = http.createServer((req, res) => {
    let pathname = decodeURIComponent(new URL(req.url, "http://127.0.0.1").pathname);
    if (pathname === "/") {
      pathname = "/index.html";
    }

    const file = path.resolve(path.join(root, pathname));
    if (!file.startsWith(root + path.sep) && file !== root) {
      res.writeHead(403).end("forbidden");
      return;
    }

    fs.readFile(file, (err, data) => {
      if (err) {
        res.writeHead(404).end(String(err));
        return;
      }
      res.writeHead(200, {
        "Content-Type": contentType(file),
        "Cross-Origin-Opener-Policy": "same-origin",
        "Cross-Origin-Embedder-Policy": "require-corp",
      });
      res.end(data);
    });
  });

  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });

  return {
    server,
    url: `http://127.0.0.1:${server.address().port}/`,
  };
}

function findChrome() {
  const candidates = [
    process.env.CHROME,
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
  ].filter(Boolean);

  for (const candidate of candidates) {
    if (candidate.includes(path.sep) && fs.existsSync(candidate)) {
      return candidate;
    }
    const found = spawnSync("which", [candidate], { encoding: "utf8" });
    if (found.status === 0 && found.stdout.trim()) {
      return found.stdout.trim();
    }
  }

  throw new Error("could not find Chrome; set CHROME=/path/to/chrome");
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

async function removeDirWithRetry(dir) {
  for (let attempt = 0; attempt < 10; attempt += 1) {
    try {
      fs.rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
      return;
    } catch (err) {
      if (attempt === 9) {
        console.warn(`warning: failed to remove ${dir}: ${err.message}`);
        return;
      }
      await sleep(100);
    }
  }
}

async function launchChrome() {
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "ruwasm-smoke-chrome-"));
  const chrome = spawn(
    findChrome(),
    [
      "--headless=new",
      "--no-sandbox",
      "--disable-gpu",
      "--disable-dev-shm-usage",
      "--remote-debugging-port=0",
      `--user-data-dir=${userDataDir}`,
      "about:blank",
    ],
    { stdio: ["ignore", "ignore", "pipe"] },
  );

  chrome.stderr.setEncoding("utf8");
  let stderr = "";
  const wsUrl = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reject(new Error(`timed out waiting for Chrome DevTools URL\n${stderr}`));
    }, 10_000);

    chrome.stderr.on("data", (chunk) => {
      stderr += chunk;
      const match = stderr.match(/DevTools listening on (ws:\/\/[^\s]+)/);
      if (match) {
        clearTimeout(timer);
        resolve(match[1]);
      }
    });

    chrome.once("exit", (code, signal) => {
      clearTimeout(timer);
      reject(new Error(`Chrome exited before DevTools was ready: ${code ?? signal}\n${stderr}`));
    });
  });

  return {
    chrome,
    userDataDir,
    wsUrl,
    async close() {
      if (!chrome.killed) {
        chrome.kill("SIGTERM");
      }
      await Promise.race([
        new Promise((resolve) => chrome.once("exit", resolve)),
        sleep(2_000),
      ]);
      if (chrome.exitCode === null && chrome.signalCode === null) {
        chrome.kill("SIGKILL");
        await Promise.race([
          new Promise((resolve) => chrome.once("exit", resolve)),
          sleep(1_000),
        ]);
      }
      await removeDirWithRetry(userDataDir);
    },
  };
}

class DevToolsWebSocket {
  constructor(wsUrl) {
    this.url = new URL(wsUrl);
    this.socket = null;
    this.buffer = Buffer.alloc(0);
    this.messages = [];
    this.waiters = [];
    this.closed = false;
  }

  async connect() {
    if (this.url.protocol !== "ws:") {
      throw new Error(`unsupported DevTools protocol: ${this.url.protocol}`);
    }

    const port = Number(this.url.port || 80);
    this.socket = net.createConnection({ host: this.url.hostname, port });
    await new Promise((resolve, reject) => {
      this.socket.once("connect", resolve);
      this.socket.once("error", reject);
    });

    const key = crypto.randomBytes(16).toString("base64");
    const request = [
      `GET ${this.url.pathname}${this.url.search} HTTP/1.1`,
      `Host: ${this.url.host}`,
      "Upgrade: websocket",
      "Connection: Upgrade",
      `Sec-WebSocket-Key: ${key}`,
      "Sec-WebSocket-Version: 13",
      "",
      "",
    ].join("\r\n");
    this.socket.write(request);

    const remainder = await this.readHandshake();
    this.socket.on("data", (chunk) => this.readFrames(chunk));
    this.socket.on("close", () => {
      this.closed = true;
      while (this.waiters.length > 0) {
        this.waiters.shift().reject(new Error("DevTools WebSocket closed"));
      }
    });
    if (remainder.length > 0) {
      this.readFrames(remainder);
    }
  }

  readHandshake() {
    return new Promise((resolve, reject) => {
      let data = Buffer.alloc(0);
      const onData = (chunk) => {
        data = Buffer.concat([data, chunk]);
        const end = data.indexOf("\r\n\r\n");
        if (end === -1) {
          return;
        }

        this.socket.off("data", onData);
        const header = data.subarray(0, end).toString("utf8");
        if (!header.startsWith("HTTP/1.1 101")) {
          reject(new Error(`DevTools WebSocket upgrade failed:\n${header}`));
          return;
        }
        resolve(data.subarray(end + 4));
      };

      this.socket.on("data", onData);
      this.socket.once("error", reject);
    });
  }

  readFrames(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);

    while (this.buffer.length >= 2) {
      const first = this.buffer[0];
      const second = this.buffer[1];
      const opcode = first & 0x0f;
      const masked = (second & 0x80) !== 0;
      let length = second & 0x7f;
      let offset = 2;

      if (length === 126) {
        if (this.buffer.length < offset + 2) {
          return;
        }
        length = this.buffer.readUInt16BE(offset);
        offset += 2;
      } else if (length === 127) {
        if (this.buffer.length < offset + 8) {
          return;
        }
        const bigLength = this.buffer.readBigUInt64BE(offset);
        if (bigLength > BigInt(Number.MAX_SAFE_INTEGER)) {
          throw new Error("DevTools WebSocket frame is too large");
        }
        length = Number(bigLength);
        offset += 8;
      }

      let mask;
      if (masked) {
        if (this.buffer.length < offset + 4) {
          return;
        }
        mask = this.buffer.subarray(offset, offset + 4);
        offset += 4;
      }

      if (this.buffer.length < offset + length) {
        return;
      }

      let payload = this.buffer.subarray(offset, offset + length);
      this.buffer = this.buffer.subarray(offset + length);

      if (masked) {
        payload = Buffer.from(payload.map((byte, index) => byte ^ mask[index % 4]));
      }

      if (opcode === 1) {
        this.deliver(payload.toString("utf8"));
      } else if (opcode === 8) {
        this.close();
      } else if (opcode === 9) {
        this.sendFrame(0x0a, payload);
      }
    }
  }

  deliver(message) {
    const waiter = this.waiters.shift();
    if (waiter) {
      waiter.resolve(message);
    } else {
      this.messages.push(message);
    }
  }

  async nextMessage() {
    if (this.messages.length > 0) {
      return this.messages.shift();
    }
    if (this.closed) {
      throw new Error("DevTools WebSocket closed");
    }
    return new Promise((resolve, reject) => this.waiters.push({ resolve, reject }));
  }

  sendText(text) {
    this.sendFrame(0x01, Buffer.from(text, "utf8"));
  }

  sendFrame(opcode, payload) {
    const mask = crypto.randomBytes(4);
    let header;
    if (payload.length < 126) {
      header = Buffer.from([0x80 | opcode, 0x80 | payload.length]);
    } else if (payload.length < 65_536) {
      header = Buffer.alloc(4);
      header[0] = 0x80 | opcode;
      header[1] = 0x80 | 126;
      header.writeUInt16BE(payload.length, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x80 | opcode;
      header[1] = 0x80 | 127;
      header.writeBigUInt64BE(BigInt(payload.length), 2);
    }

    const maskedPayload = Buffer.from(payload);
    for (let i = 0; i < maskedPayload.length; i += 1) {
      maskedPayload[i] ^= mask[i % 4];
    }
    this.socket.write(Buffer.concat([header, mask, maskedPayload]));
  }

  close() {
    if (!this.closed) {
      this.closed = true;
      this.socket.end();
    }
  }
}

class Cdp {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.handlers = [];
  }

  start() {
    this.readLoop().catch((err) => {
      for (const pending of this.pending.values()) {
        pending.reject(err);
      }
      this.pending.clear();
    });
  }

  async readLoop() {
    while (!this.socket.closed) {
      const message = JSON.parse(await this.socket.nextMessage());
      if (message.id && this.pending.has(message.id)) {
        const pending = this.pending.get(message.id);
        this.pending.delete(message.id);
        clearTimeout(pending.timer);
        if (message.error) {
          pending.reject(new Error(`${pending.method}: ${message.error.message}`));
        } else {
          pending.resolve(message.result);
        }
      } else {
        for (const handler of this.handlers) {
          handler(message);
        }
      }
    }
  }

  onEvent(handler) {
    this.handlers.push(handler);
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId++;
    const message = { id, method, params };
    if (sessionId) {
      message.sessionId = sessionId;
    }

    this.socket.sendText(JSON.stringify(message));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out`));
      }, 15_000);
      this.pending.set(id, { resolve, reject, method, timer });
    });
  }
}

function exceptionText(details) {
  return details?.exception?.description || details?.text || JSON.stringify(details);
}

async function runSmokeTest(appUrl, cdp) {
  const target = await cdp.send("Target.createTarget", { url: "about:blank" });
  const attached = await cdp.send("Target.attachToTarget", {
    targetId: target.targetId,
    flatten: true,
  });
  const sessionId = attached.sessionId;
  const pageErrors = [];

  cdp.onEvent((message) => {
    if (message.sessionId !== sessionId) {
      return;
    }
    if (message.method === "Runtime.exceptionThrown") {
      pageErrors.push(exceptionText(message.params.exceptionDetails));
    }
    if (message.method === "Runtime.consoleAPICalled" && message.params.type === "error") {
      pageErrors.push(message.params.args.map((arg) => arg.value || arg.description).join(" "));
    }
    if (
      message.method === "Log.entryAdded" &&
      message.params.entry.source !== "network" &&
      ["error", "fatal"].includes(message.params.entry.level)
    ) {
      pageErrors.push(message.params.entry.text);
    }
  });

  await cdp.send("Runtime.enable", {}, sessionId);
  await cdp.send("Log.enable", {}, sessionId);
  await cdp.send("Page.enable", {}, sessionId);
  await cdp.send("Page.navigate", { url: appUrl }, sessionId);

  const expression = `
    new Promise((resolve, reject) => {
      const started = Date.now();
      const timeoutMs = 10000;
      function summary() {
        const root = document.querySelector("#time-sink");
        return {
          readyState: document.readyState,
          crossOriginIsolated: window.crossOriginIsolated,
          rootHtml: root ? root.innerHTML.slice(0, 600) : null,
          result: document.querySelector("#result")?.textContent || null,
        };
      }
      function check() {
        const root = document.querySelector("#time-sink");
        const pause = root?.querySelector("[data-role='pause']");
        const auto = root?.querySelector("[data-role='y-auto']");
        const canvas = root?.querySelector("canvas.rr-time-sink-canvas");
        if (root && pause && auto && canvas) {
          resolve({
            crossOriginIsolated: window.crossOriginIsolated,
            pauseText: pause.textContent,
            autoText: auto.textContent,
            canvasClass: canvas.className,
            resultText: document.querySelector("#result")?.textContent || "",
          });
          return;
        }
        if (Date.now() - started > timeoutMs) {
          reject(new Error("timed out waiting for generated time sink DOM: " + JSON.stringify(summary())));
          return;
        }
        setTimeout(check, 50);
      }
      check();
    })
  `;

  const evaluated = await cdp.send(
    "Runtime.evaluate",
    {
      expression,
      awaitPromise: true,
      returnByValue: true,
    },
    sessionId,
  );

  if (evaluated.exceptionDetails) {
    throw new Error(exceptionText(evaluated.exceptionDetails));
  }

  const value = evaluated.result.value;
  if (!value.crossOriginIsolated) {
    throw new Error("page is not cross-origin isolated");
  }
  if (value.pauseText !== "Pause") {
    throw new Error(`unexpected pause button text: ${value.pauseText}`);
  }
  if (value.autoText !== "Autoscale On") {
    throw new Error(`unexpected autoscale button text: ${value.autoText}`);
  }
  if (pageErrors.length > 0) {
    throw new Error(`page reported error(s):\n${pageErrors.join("\n")}`);
  }

  console.log("browser smoke ok:", JSON.stringify(value));
}

let server;
let chrome;
let socket;
try {
  const staticServer = await startStaticServer(resolvedSiteRoot);
  server = staticServer.server;
  chrome = await launchChrome();
  socket = new DevToolsWebSocket(chrome.wsUrl);
  await socket.connect();
  const cdp = new Cdp(socket);
  cdp.start();
  await runSmokeTest(staticServer.url, cdp);
} finally {
  if (socket) {
    socket.close();
  }
  if (chrome) {
    await chrome.close();
  }
  if (server) {
    await new Promise((resolve) => server.close(resolve));
  }
}
