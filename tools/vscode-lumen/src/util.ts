// Shared helpers: locate a Lumen app directory and quote shell arguments.

import * as fs from "fs";
import * as path from "path";
import { Uri, window, workspace } from "vscode";

/** A Lumen app dir is any directory that contains `main.lmn`. */
export function isAppDir(dir: string): boolean {
    try {
        return fs.existsSync(path.join(dir, "main.lmn"));
    } catch {
        return false;
    }
}

/**
 * Walk up from `start` looking for the nearest ancestor that contains
 * `main.lmn`. Returns undefined if none is found before the filesystem root.
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
 * then the single workspace folder, then prompts across `main.lmn` matches.
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

    // Fall back to discovering every main.lmn in the workspace.
    const hits = await workspace.findFiles("**/main.lmn", "**/node_modules/**", 50);
    const dirs = Array.from(new Set(hits.map((u: Uri) => path.dirname(u.fsPath)))).sort();
    if (dirs.length === 0) {
        window.showErrorMessage(
            "No Lumen app found. A Lumen app is a directory containing main.lmn.",
        );
        return undefined;
    }
    if (dirs.length === 1) {
        return dirs[0];
    }
    return window.showQuickPick(dirs, {
        placeHolder: "Select the Lumen app directory (contains main.lmn)",
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
