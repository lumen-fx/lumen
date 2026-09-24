#!/usr/bin/env node
// A cargo runner for the browser suites when they are built for coverage.
//
// `wasm-bindgen-test-runner` collects coverage by having the page POST the
// counters back to its own server once the tests finish, but in headless mode
// it stops watching the page as soon as the summary line appears and exits,
// usually before that POST has been written to disk. What is left is an empty
// or missing .profraw and a report that reads zero for code the suite ran.
//
// So this runner keeps the parts of `wasm-bindgen-test-runner` that matter
// (it still generates the bindings and serves the page, in its interactive
// mode) and drives Chrome itself over WebDriver, which lets it wait for the
// coverage request to complete before anything shuts down. Outside coverage
// runs, use `wasm-bindgen-test-runner` directly; it is what ci.yml runs.
//
// Cargo invokes it as `node wasm-coverage-runner.mjs <test.wasm> [args...]`.
// It reads the same environment the stock runner does: `CHROMEDRIVER` for the
// driver binary, `LLVM_PROFILE_FILE` (passed through) for where the counters
// land, and optionally `CHROME_BIN` for the browser.

import { spawn } from "node:child_process";
import { createServer } from "node:net";
import { setTimeout as sleep } from "node:timers/promises";

const TEST_TIMEOUT_MS = 600_000;
const COVERAGE_TIMEOUT_MS = 300_000;

const [wasm, ...args] = process.argv.slice(2);
if (!wasm) {
  console.error("usage: wasm-coverage-runner.mjs <test.wasm> [args...]");
  process.exit(2);
}

const children = [];
function cleanup() {
  for (const child of children) {
    if (child.exitCode === null) child.kill("SIGTERM");
  }
}
process.on("exit", cleanup);

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

// Start the stock runner in interactive mode and wait for it to say where the
// page is. A suite configured for Node instead of a browser never serves a
// page: the stock runner then runs it itself and writes the counters before it
// exits, so its exit status is passed straight through.
async function servePage() {
  const port = await freePort();
  const runner = spawn("wasm-bindgen-test-runner", [wasm, ...args], {
    env: {
      ...process.env,
      NO_HEADLESS: "1",
      WASM_BINDGEN_TEST_ADDRESS: `127.0.0.1:${port}`,
    },
    stdio: ["ignore", "pipe", "inherit"],
  });
  children.push(runner);
  return new Promise((resolve) => {
    let seen = "";
    let serving = false;
    const onData = (chunk) => {
      seen += chunk;
      const url = seen.match(/available at (http:\/\/\S+)/);
      if (url) {
        serving = true;
        runner.stdout.off("data", onData);
        runner.stdout.resume();
        resolve(url[1]);
      }
    };
    runner.stdout.setEncoding("utf8");
    runner.stdout.on("data", onData);
    runner.on("exit", (code) => {
      if (serving) {
        console.error("wasm-bindgen-test-runner stopped serving the page");
        process.exit(1);
      }
      process.stdout.write(seen);
      process.exit(code ?? 1);
    });
  });
}

async function startDriver() {
  const port = await freePort();
  const driver = spawn(process.env.CHROMEDRIVER || "chromedriver", [`--port=${port}`], {
    stdio: "ignore",
  });
  children.push(driver);
  const base = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + 30_000;
  while (Date.now() < deadline) {
    try {
      const status = await fetch(`${base}/status`).then((r) => r.json());
      if (status.value?.ready) return base;
    } catch {
      // Not listening yet.
    }
    await sleep(100);
  }
  throw new Error("chromedriver did not come up within 30 seconds");
}

async function webdriver(base, method, path, body) {
  const response = await fetch(`${base}${path}`, {
    method,
    headers: { "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const reply = await response.json();
  if (!response.ok) {
    throw new Error(`WebDriver ${method} ${path}: ${JSON.stringify(reply.value)}`);
  }
  return reply.value;
}

const url = await servePage();
const driver = await startDriver();

const chromeOptions = {
  args: ["--headless=new", "--no-sandbox", "--disable-gpu", "--disable-dev-shm-usage"],
};
if (process.env.CHROME_BIN) chromeOptions.binary = process.env.CHROME_BIN;
const session = await webdriver(driver, "POST", "/session", {
  capabilities: { alwaysMatch: { "goog:chromeOptions": chromeOptions } },
});
const at = `/session/${session.sessionId}`;
const run = (script) => webdriver(driver, "POST", `${at}/execute/sync`, { script, args: [] });

await webdriver(driver, "POST", `${at}/url`, { url });

// Stream the page's output until the harness prints its summary.
let printed = 0;
let output = "";
const testDeadline = Date.now() + TEST_TIMEOUT_MS;
while (!output.includes("test result: ")) {
  if (Date.now() > testDeadline) {
    console.error("the suite did not finish in time");
    process.exit(1);
  }
  await sleep(100);
  output = (await run("return document.getElementById('output')?.textContent ?? ''")) || "";
  process.stdout.write(output.slice(printed));
  printed = output.length;
}

// The page sends the counters right after the summary; the resource timing
// entry for that request appears once the server has answered, and the server
// answers after it has written the file.
const coverageDeadline = Date.now() + COVERAGE_TIMEOUT_MS;
let status = 0;
while (status === 0) {
  if (Date.now() > coverageDeadline) {
    console.error("the page never finished sending its coverage counters");
    process.exit(1);
  }
  await sleep(100);
  status = await run(`
    const entry = performance.getEntriesByType('resource')
      .find((e) => e.name.endsWith('/__wasm_bindgen/coverage'));
    return entry ? (entry.responseStatus || 1) : 0;
  `);
}
if (status >= 400) {
  console.error(`the runner refused the coverage counters (HTTP ${status})`);
  process.exit(1);
}

const passed = output.includes("test result: ok");
if (!passed) {
  const consoleOutput = await run(
    "return document.getElementById('console_output')?.textContent ?? ''",
  );
  if (consoleOutput) process.stdout.write(`console output:\n${consoleOutput}\n`);
}
await webdriver(driver, "DELETE", at).catch(() => {});
process.exit(passed ? 0 : 1);
