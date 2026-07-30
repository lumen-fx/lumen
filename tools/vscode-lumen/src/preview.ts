// In-editor live preview - the polish differentiator.
//
// Approach (inspired by the Slint extension's live-preview panel, adapted to
// Lumen's headless + MCP model, which Lumen wins on):
//
//   1. Spawn `lumenc run <dir> --headless [--size WxH] [--dpr N]`. Headless mode
//      runs the FULL pipeline (layout + GPU render + MCP TCP server) with ZERO
//      windows - it never touches the desktop - then idles.
//   2. Poll `lumenc screenshot <tmp.png> --app <dir>` (which drives the MCP
//      `lumen.screenshot` method over TCP) until the server is up and returns a
//      frame.
//   3. Decode the PNG and show it in a themed WebviewPanel. "Refresh"
//      re-screenshots the still-running app; closing the panel kills the child.
//
// Slint renders via its own interpreter in-process; Lumen instead reuses the
// real headless runtime + MCP screenshot path so the preview is pixel-identical
// to `lumenc run --headless` output - no second renderer to keep in sync.

import { ChildProcess, execFile, spawn } from "child_process";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import {
    ExtensionContext,
    ViewColumn,
    Webview,
    WebviewPanel,
    window,
    workspace,
} from "vscode";
import { resolveBinary } from "./config";
import { resolveAppDir } from "./util";

let current: PreviewSession | undefined;

export async function openPreview(_ctx: ExtensionContext): Promise<void> {
    const dir = await resolveAppDir();
    if (!dir) {
        return;
    }
    if (current) {
        current.retarget(dir);
        current.reveal();
        return;
    }
    const panel = window.createWebviewPanel(
        "lumenPreview",
        `Lumen Preview - ${path.basename(dir)}`,
        ViewColumn.Beside,
        { enableScripts: true, retainContextWhenHidden: true },
    );
    current = new PreviewSession(panel, dir);
    panel.onDidDispose(() => {
        current = undefined;
    });
}

function lumenc(): string {
    return resolveBinary("lumenc", "lumencPath").command;
}

class PreviewSession {
    private child: ChildProcess | undefined;
    private tmpPng: string;
    private disposed = false;

    constructor(private panel: WebviewPanel, private dir: string) {
        this.tmpPng = path.join(
            os.tmpdir(),
            `lumen-preview-${process.pid}-${Date.now()}.png`,
        );
        this.panel.webview.html = shell(this.panel.webview);
        this.panel.webview.onDidReceiveMessage((m) => {
            if (m?.type === "refresh") {
                void this.capture();
            }
        });
        this.panel.onDidDispose(() => this.dispose());
        void this.launch();
    }

    reveal(): void {
        this.panel.reveal(ViewColumn.Beside);
    }

    retarget(dir: string): void {
        if (dir === this.dir) {
            return;
        }
        this.dir = dir;
        this.panel.title = `Lumen Preview - ${path.basename(dir)}`;
        this.stopChild();
        void this.launch();
    }

    private post(state: string, detail = "", png?: string): void {
        void this.panel.webview.postMessage({ type: "state", state, detail, png });
    }

    private async launch(): Promise<void> {
        this.post("loading", "Starting headless runtime...");
        const cfg = workspace.getConfiguration("lumen");
        const size = cfg.get<string>("preview.size", "960x720");
        const dpr = String(cfg.get<number>("preview.dpr", 1));
        const args = ["run", this.dir, "--headless", "--size", size, "--dpr", dpr];

        try {
            this.child = spawn(lumenc(), args, {
                cwd: this.dir,
                stdio: ["ignore", "pipe", "pipe"],
            });
        } catch (e) {
            this.post("error", `Failed to spawn lumenc: ${e}`);
            return;
        }
        this.child.on("error", (e) =>
            this.post("error", `lumenc run failed to launch: ${e.message}`),
        );
        this.child.on("exit", (code) => {
            if (!this.disposed && code !== 0 && code !== null) {
                this.post("error", `Headless runtime exited (code ${code}).`);
            }
        });

        // Give the MCP server a moment to bind, then poll the screenshot path.
        await this.capture(20);
    }

    /** Try to capture a frame, retrying while the MCP server comes up. */
    private async capture(retries = 6): Promise<void> {
        for (let i = 0; i < retries && !this.disposed; i++) {
            await delay(i === 0 ? 500 : 400);
            const ok = await this.screenshotOnce();
            if (ok) {
                return;
            }
        }
        if (!this.disposed) {
            this.post(
                "error",
                "Could not capture a frame. Ensure [mcp] is enabled in lumen.toml and lumenc is on PATH (or set lumen.lumencPath).",
            );
        }
    }

    private screenshotOnce(): Promise<boolean> {
        return new Promise((resolve) => {
            execFile(
                lumenc(),
                ["screenshot", this.tmpPng, "--app", this.dir],
                { cwd: this.dir },
                (err) => {
                    if (err) {
                        resolve(false);
                        return;
                    }
                    try {
                        const buf = fs.readFileSync(this.tmpPng);
                        this.post(
                            "ready",
                            new Date().toLocaleTimeString(),
                            `data:image/png;base64,${buf.toString("base64")}`,
                        );
                        resolve(true);
                    } catch {
                        resolve(false);
                    }
                },
            );
        });
    }

    private stopChild(): void {
        if (this.child) {
            try {
                this.child.kill();
            } catch {
                /* already gone */
            }
            this.child = undefined;
        }
    }

    private dispose(): void {
        this.disposed = true;
        this.stopChild();
        fs.rm(this.tmpPng, () => undefined);
    }
}

function delay(ms: number): Promise<void> {
    return new Promise((r) => setTimeout(r, ms));
}

// Webview chrome is styled entirely from VS Code theme tokens (var(--vscode-*))
// so it tracks the editor's light/dark theme with no hardcoded colors.
function shell(webview: Webview): string {
    const nonce = Array.from({ length: 24 }, () =>
        "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789".charAt(
            Math.floor(Math.random() * 62),
        ),
    ).join("");
    const csp = [
        "default-src 'none'",
        "img-src data:",
        `style-src 'nonce-${nonce}'`,
        `script-src 'nonce-${nonce}'`,
    ].join("; ");
    return `<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8" />
<meta http-equiv="Content-Security-Policy" content="${csp}" />
<style nonce="${nonce}">
  :root { color-scheme: light dark; }
  body {
    margin: 0;
    font-family: var(--vscode-font-family);
    color: var(--vscode-foreground);
    background: var(--vscode-editor-background);
  }
  .bar {
    display: flex; align-items: center; gap: 10px;
    padding: 6px 10px;
    border-bottom: 1px solid var(--vscode-panel-border);
    background: var(--vscode-editorGroupHeader-tabsBackground);
    position: sticky; top: 0;
  }
  button {
    font: inherit;
    color: var(--vscode-button-foreground);
    background: var(--vscode-button-background);
    border: none; padding: 4px 12px; border-radius: 3px; cursor: pointer;
  }
  button:hover { background: var(--vscode-button-hoverBackground); }
  .status { opacity: 0.75; font-size: 12px; }
  .stage {
    display: flex; align-items: center; justify-content: center;
    padding: 16px; min-height: 200px;
  }
  img {
    max-width: 100%; height: auto;
    box-shadow: 0 2px 12px rgba(0,0,0,0.35);
    border: 1px solid var(--vscode-panel-border);
  }
  .msg { opacity: 0.8; padding: 40px; text-align: center; }
  .err { color: var(--vscode-errorForeground); }
</style>
</head>
<body>
  <div class="bar">
    <button id="refresh">Refresh</button>
    <span class="status" id="status">Starting...</span>
  </div>
  <div class="stage" id="stage">
    <div class="msg" id="msg">Booting headless runtime...</div>
  </div>
<script nonce="${nonce}">
  const vscode = acquireVsCodeApi();
  const stage = document.getElementById('stage');
  const msg = document.getElementById('msg');
  const status = document.getElementById('status');
  document.getElementById('refresh').addEventListener('click', () => {
    status.textContent = 'Refreshing...';
    vscode.postMessage({ type: 'refresh' });
  });
  window.addEventListener('message', (e) => {
    const d = e.data || {};
    if (d.type !== 'state') return;
    if (d.state === 'ready' && d.png) {
      status.textContent = 'Captured ' + d.detail;
      stage.innerHTML = '';
      const img = document.createElement('img');
      img.src = d.png;
      stage.appendChild(img);
    } else if (d.state === 'error') {
      status.textContent = 'Error';
      stage.innerHTML = '<div class="msg err">' + d.detail + '</div>';
    } else {
      status.textContent = d.detail || 'Loading...';
      if (!stage.querySelector('img')) {
        stage.innerHTML = '<div class="msg">' + (d.detail || 'Loading...') + '</div>';
      }
    }
  });
</script>
</body>
</html>`;
}
