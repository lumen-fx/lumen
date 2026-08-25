// Shared helpers: locate a Lumen app directory and quote shell arguments.

import * as fs from "fs";
import * as path from "path";
import { Uri, window, workspace } from "vscode";

/** A Lumen app root holds `lumen.toml`, or holds `src/main.lmn`. */
export function isAppDir(dir: string): boolean {
    try {
        return (
            fs.existsSync(path.join(dir, "lumen.toml")) ||
            fs.existsSync(path.join(dir, "src", "main.lmn"))
        );
    } catch {
        return false;
    }
}

/**
 * A directory holds an app's markup when it contains `main.lmn`, or when it is
 * the `src` of an app root. The second case covers multi-page apps, whose
 * `src` may name its pages something other than `main.lmn`.
 */
export function isMarkupDir(dir: string): boolean {
    try {
        return (
            fs.existsSync(path.join(dir, "main.lmn")) ||
            (path.basename(dir) === "src" && isAppDir(path.dirname(dir)))
        );
    } catch {
        return false;
    }
}

/**
 * Walk up from `start` looking for the nearest ancestor that is a Lumen app
 * root. Returns undefined if none is found before the filesystem root.
 */
export function findAppDirUpward(start: string): string | undefined {
    let dir = start;
    for (;;) {
        if (isAppDir(dir)) {
            return dir;
        }
        const parent = path.dirname(dir);
        if (parent === dir) {
            return undefined;
        }
        dir = parent;
    }
}

/**
 * Resolve the app directory to act on. Prefers the active editor's file,
 * then the single workspace folder, then prompts across discovered app roots.
 */
export async function resolveAppDir(): Promise<string | undefined> {
    const active = window.activeTextEditor?.document.uri;
    if (active && active.scheme === "file") {
        const found = findAppDirUpward(path.dirname(active.fsPath));
        if (found) {
            return found;
        }
    }

    const folders = workspace.workspaceFolders ?? [];
    const roots = folders.filter((f) => isAppDir(f.uri.fsPath));
    if (roots.length === 1) {
        return roots[0].uri.fsPath;
    }

    // Fall back to discovering every app root marker in the workspace.
    const [manifests, entries]: [Uri[], Uri[]] = await Promise.all([
        workspace.findFiles("**/lumen.toml", "**/node_modules/**", 50),
        workspace.findFiles("**/src/main.lmn", "**/node_modules/**", 50),
    ]);
    const found = new Set<string>();
    for (const u of manifests) {
        found.add(path.dirname(u.fsPath));
    }
    for (const u of entries) {
        found.add(path.dirname(path.dirname(u.fsPath)));
    }
    const dirs = Array.from(found).sort();
    if (dirs.length === 0) {
        window.showErrorMessage(
            "No Lumen app found. A Lumen app is a directory holding lumen.toml or src/main.lmn.",
        );
        return undefined;
    }
    if (dirs.length === 1) {
        return dirs[0];
    }
    return window.showQuickPick(dirs, {
        placeHolder: "Select the Lumen app directory (holds lumen.toml or src/main.lmn)",
    });
}

/** Minimal POSIX/Windows-safe single argument quoting for terminal use. */
export function shQuote(arg: string): string {
    if (process.platform === "win32") {
        return /[\s"]/.test(arg) ? `"${arg.replace(/"/g, '""')}"` : arg;
    }
    if (arg === "" || /[^A-Za-z0-9_@%+=:,./-]/.test(arg)) {
        return `'${arg.replace(/'/g, `'\\''`)}'`;
    }
    return arg;
}
