import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";

export async function launchChrome(executable, { startupTimeoutMs = 60_000 } = {}) {
  const userDataDir = fs.mkdtempSync(path.join(os.tmpdir(), "ruwasm-smoke-chrome-"));
  const chrome = spawn(executable, [
    "--headless=new",
    "--no-sandbox",
    "--disable-gpu",
    "--disable-dev-shm-usage",
    "--disable-background-networking",
    "--no-first-run",
    "--no-default-browser-check",
    "--remote-debugging-port=0",
    `--user-data-dir=${userDataDir}`,
    "about:blank",
  ], { stdio: ["ignore", "ignore", "pipe"] });
  const closed = new Promise((resolve) => chrome.once("close", resolve));
  let stderr = "";
  chrome.stderr.setEncoding("utf8");
  chrome.stderr.on("data", (chunk) => { stderr += chunk; });

  async function close() {
    if (chrome.pid && chrome.exitCode === null && chrome.signalCode === null) {
      chrome.kill("SIGTERM");
      const timer = setTimeout(() => chrome.kill("SIGKILL"), 2_000);
      try {
        await closed;
      } finally {
        clearTimeout(timer);
      }
    } else {
      await closed;
    }
    await fs.promises.rm(userDataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }

  let cleanupReadiness;
  try {
    const wsUrl = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        reject(new Error(`timed out waiting for Chrome DevTools URL after ${startupTimeoutMs}ms\n${stderr}`));
      }, startupTimeoutMs);
      // Chrome also publishes the endpoint here, independent of stderr logging.
      const poll = setInterval(() => {
        try {
          const [port, endpoint] = fs.readFileSync(path.join(userDataDir, "DevToolsActivePort"), "utf8").trim().split("\n");
          if (/^\d+$/.test(port) && endpoint?.startsWith("/devtools/browser/")) {
            resolve(`ws://127.0.0.1:${port}${endpoint}`);
          }
        } catch (error) {
          if (error.code !== "ENOENT") reject(error);
        }
      }, 50);
      const onData = () => {
        const match = stderr.match(/DevTools listening on (ws:\/\/[^\s]+)/);
        if (match) resolve(match[1]);
      };
      const onError = (error) => reject(new Error(`could not launch Chrome: ${error.message}`, { cause: error }));
      const onExit = (code, signal) => reject(new Error(`Chrome exited before DevTools was ready: ${code ?? signal}\n${stderr}`));
      chrome.stderr.on("data", onData);
      chrome.once("error", onError);
      chrome.once("exit", onExit);
      // Attach cleanup to both outcomes so failed launches leave no timers/listeners.
      cleanupReadiness = () => {
        clearTimeout(timer);
        clearInterval(poll);
        chrome.stderr.off("data", onData);
        chrome.off("error", onError);
        chrome.off("exit", onExit);
      };
    }).finally(() => cleanupReadiness());
    return { chrome, userDataDir, wsUrl, close };
  } catch (error) {
    await close();
    throw error;
  }
}
