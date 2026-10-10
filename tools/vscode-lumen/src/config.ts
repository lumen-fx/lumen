// Settings access + binary discovery.
//
// Discovery mirrors rust-analyzer's `bootstrapServer`: an explicit setting
// wins; otherwise we probe the workspace's Cargo target directories (honoring
// $CARGO_TARGET_DIR) for a locally-built binary, then the installed toolchain:
// the directory holding `lumenc` (the `lumen.lumencPath` setting, or `lumenc`
// on $PATH) and the install prefixes `install.sh` and the Windows installer
// use. Every toolchain archive ships `lumen-lsp` beside `lumenc`, and an editor
// started from a desktop launcher often lacks the shell's $PATH. Failing all
// that we return the bare name and let the OS resolve it on $PATH. Nothing
// here spawns a process - callers surface a friendly error if the resolved
// path can't actually launch.

import * as fs from "fs";
import * as os from "os";
import * as path from "path";
import { workspace } from "vscode";

const EXE = process.platform === "win32" ? ".exe" : "";

function expandHome(p: string): string {
    if (p === "~") {
        return os.homedir();
    }
    if (p.startsWith("~/") || p.startsWith("~\\")) {
        return path.join(os.homedir(), p.slice(2));
    }
    return p;
}

/** Candidate `target/{release,debug}/<name>` locations across the workspace. */
function targetCandidates(name: string): string[] {
    const bin = `${name}${EXE}`;
    const roots: string[] = [];

    const envTarget = process.env.CARGO_TARGET_DIR;
    if (envTarget) {
        roots.push(envTarget);
    }
    for (const folder of workspace.workspaceFolders ?? []) {
        roots.push(path.join(folder.uri.fsPath, "target"));
    }

    const out: string[] = [];
    for (const root of roots) {
        // Prefer release (what `README` tells users to build) then debug.
        out.push(path.join(root, "release", bin));
        out.push(path.join(root, "debug", bin));
    }
    return out;
}

/** The first `name` executable in a $PATH directory, symlinks resolved. */
function onPath(name: string): string | undefined {
    for (const dir of (process.env.PATH ?? "").split(path.delimiter)) {
        if (dir === "") {
            continue;
        }
        const candidate = path.join(dir, `${name}${EXE}`);
        try {
            if (fs.statSync(candidate).isFile()) {
                return fs.realpathSync(candidate);
            }
        } catch {
            // Not here; keep looking.
        }
    }
    return undefined;
}

/**
 * Where an installed toolchain keeps `name`: beside `lumenc` (the one the
 * `lumen.lumencPath` setting names, or the one on $PATH), then the default
 * prefixes of `install.sh` (`~/.lumen/bin`) and the Windows installer
 * (`%LOCALAPPDATA%\Programs\Lumen\bin`).
 */
function installCandidates(name: string): string[] {
    const bin = `${name}${EXE}`;
    const dirs: string[] = [];
    const lumencSetting = (workspace.getConfiguration("lumen").get<string>("lumencPath") ?? "").trim();
    if (lumencSetting !== "") {
        const lumenc = expandHome(lumencSetting);
        try {
            dirs.push(path.dirname(fs.realpathSync(lumenc)));
        } catch {
            dirs.push(path.dirname(lumenc));
        }
    }
    const lumenc = onPath("lumenc");
    if (lumenc !== undefined) {
        dirs.push(path.dirname(lumenc));
    }
    dirs.push(path.join(os.homedir(), ".lumen", "bin"));
    if (process.platform === "win32" && process.env.LOCALAPPDATA) {
        dirs.push(path.join(process.env.LOCALAPPDATA, "Programs", "Lumen", "bin"));
    }
    return dirs.map((dir) => path.join(dir, bin));
}

export interface ResolvedBinary {
    /** Command to spawn (absolute path if discovered, else the bare name). */
    command: string;
    /** True when we found a concrete file on disk (vs. trusting $PATH). */
    discovered: boolean;
}

/**
 * Resolve a Lumen binary (`lumen-lsp` or `lumenc`).
 *
 * @param name         binary base name.
 * @param settingKey   the `lumen.*Path` setting to consult first.
 */
export function resolveBinary(name: string, settingKey: string): ResolvedBinary {
    const cfg = workspace.getConfiguration("lumen");
    const explicit = (cfg.get<string>(settingKey) ?? "").trim();
    if (explicit !== "") {
        return { command: expandHome(explicit), discovered: true };
    }

    if (cfg.get<boolean>("serverAutoDiscover", true)) {
        for (const candidate of [...targetCandidates(name), ...installCandidates(name)]) {
            try {
                if (fs.existsSync(candidate) && fs.statSync(candidate).isFile()) {
                    return { command: candidate, discovered: true };
                }
            } catch {
                // Ignore unreadable candidates and keep probing.
            }
        }
    }

    return { command: name, discovered: false };
}

export function traceLevel(): "off" | "messages" | "verbose" {
    return workspace
        .getConfiguration("lumen")
        .get<"off" | "messages" | "verbose">("trace.server", "off");
}
