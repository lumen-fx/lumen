// lumenc-backed commands. Long-running / user-visible invocations go through a
// shared integrated terminal so output streams live and the user can Ctrl-C -
// the same UX rust-analyzer uses for its cargo tasks.

import * as path from "path";
import {
    ExtensionContext,
    Terminal,
    Uri,
    commands,
    window,
    workspace,
} from "vscode";
import { resolveBinary } from "./config";
import { openPreview } from "./preview";
import { resolveAppDir, shQuote } from "./util";

let terminal: Terminal | undefined;

function lumenc(): string {
    return resolveBinary("lumenc", "lumencPath").command;
}

function runInTerminal(argv: string[], cwd?: string): void {
    if (!terminal || terminal.exitStatus !== undefined) {
        terminal = window.createTerminal({ name: "Lumen", cwd });
    }
    terminal.show(true);
    terminal.sendText(argv.map(shQuote).join(" "));
}

async function cmdRun(): Promise<void> {
    const dir = await resolveAppDir();
    if (!dir) {
        return;
    }
    const cfg = workspace.getConfiguration("lumen");
    const argv = [lumenc(), "run", dir];
    if (cfg.get<boolean>("run.headless", false)) {
        argv.push("--headless");
    }
    argv.push(...cfg.get<string[]>("run.flags", []));
    runInTerminal(argv, dir);
}

async function cmdCheck(): Promise<void> {
    const dir = await resolveAppDir();
    if (!dir) {
        return;
    }
    runInTerminal([lumenc(), "check", dir], dir);
}

async function cmdFormat(): Promise<void> {
    const doc = window.activeTextEditor?.document;
    if (!doc || doc.languageId !== "lumen-markup") {
        window.showErrorMessage("Open a .lmn markup file to format.");
        return;
    }
    if (doc.isDirty) {
        await doc.save();
    }
    runInTerminal([lumenc(), "fmt", doc.uri.fsPath], path.dirname(doc.uri.fsPath));
}

async function cmdBuild(): Promise<void> {
    const dir = await resolveAppDir();
    if (!dir) {
        return;
    }
    const out = `${path.basename(dir)}.lmna`;
    // `lumenc build` ahead-of-time compiles an app dir into a single .lmna
    // artifact (parsed + cascaded IR + baked scripts). A parser-free runtime
    // loads it via `lumenc run <dir> --artifact <out>` (mirrors Qt qmlcachegen
    // / Slint compile-at-build).
    runInTerminal([lumenc(), "build", dir, path.join(dir, out)], dir);
}

interface TemplateItem {
    label: string;
    description: string;
}

// Kept in sync with lumenc's scaffold::TEMPLATES gallery.
const TEMPLATES: TemplateItem[] = [
    { label: "blank", description: "Empty starting point: a bare <root> and a lumen.toml." },
    { label: "hello", description: "Minimal one-screen app." },
    { label: "counter", description: "Buttons, bind-text, per-id click routing." },
    { label: "form", description: "Two-way bound form: input, toggle, slider." },
    { label: "todo", description: "List + input + <for> loop + array signals." },
    { label: "dashboard", description: "Multi-panel layout with derived signals." },
    { label: "settings", description: "Settings/login panel scaffold." },
    { label: "hotkeys", description: "Native shell: global hotkeys, tray, notifications." },
];

async function cmdNewFromTemplate(): Promise<void> {
    const pick = await window.showQuickPick(
        TEMPLATES.map((t) => ({ label: t.label, detail: t.description })),
        { placeHolder: "Choose a Lumen template" },
    );
    if (!pick) {
        return;
    }
    const name = await window.showInputBox({
        prompt: `New app directory name (from '${pick.label}')`,
        value: pick.label + "-app",
        validateInput: (v) =>
            /^[A-Za-z0-9._-]+$/.test(v) ? null : "Use letters, digits, . _ -",
    });
    if (!name) {
        return;
    }
    const folder = workspace.workspaceFolders?.[0]?.uri.fsPath ?? process.cwd();
    runInTerminal([lumenc(), "new", name, pick.label], folder);

    // Offer to open the scaffolded main.lmn once the CLI has written it.
    const target = Uri.file(path.join(folder, name, "src", "main.lmn"));
    setTimeout(() => {
        workspace.fs.stat(target).then(
            () => {
                void window
                    .showInformationMessage(`Created '${name}'.`, "Open main.lmn")
                    .then((c) => {
                        if (c === "Open main.lmn") {
                            void window.showTextDocument(target);
                        }
                    });
            },
            () => {
                /* scaffold still running or failed; terminal shows why. */
            },
        );
    }, 1500);
}

export function registerCommands(ctx: ExtensionContext, restart: () => Promise<void>): void {
    ctx.subscriptions.push(
        commands.registerCommand("lumen.run", cmdRun),
        commands.registerCommand("lumen.check", cmdCheck),
        commands.registerCommand("lumen.format", cmdFormat),
        commands.registerCommand("lumen.build", cmdBuild),
        commands.registerCommand("lumen.newFromTemplate", cmdNewFromTemplate),
        commands.registerCommand("lumen.preview", () => openPreview(ctx)),
        commands.registerCommand("lumen.restartServer", restart),
        {
            dispose: () => {
                terminal?.dispose();
                terminal = undefined;
            },
        },
    );
}
