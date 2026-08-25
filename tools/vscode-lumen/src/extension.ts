// Lumen VS Code extension entry point.
//
// Responsibilities:
//   - Boot the `lumen-lsp` language server (see client.ts) and expose a restart.
//   - Register the lumenc-backed commands (see commands.ts) and the headless
//     live-preview panel (see preview.ts).
//   - Retag Lumen-adjacent `.css` files (a stylesheet sitting in an app's
//     markup directory) as the `lumen-css` language so the Lumen CSS grammar
//     applies without hijacking every `.css` file in unrelated projects.
//
// Language intelligence is NOT reimplemented here - it lives in the Rust
// `lumen-lsp` server. This file only transports it and adds editor glue.

import * as path from "path";
import {
    ExtensionContext,
    TextDocument,
    languages,
    workspace,
} from "vscode";
import { LumenServer } from "./client";
import { registerCommands } from "./commands";
import { isMarkupDir } from "./util";

let server: LumenServer | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
    server = new LumenServer(context);
    registerCommands(context, () => server!.start());

    // Scoped CSS retagging: only stylesheets that live beside an app's markup
    // become `lumen-css`. Global CSS stays untouched.
    context.subscriptions.push(
        workspace.onDidOpenTextDocument(maybeRetagCss),
        {
            dispose: async () => {
                await server?.stop();
            },
        },
    );
    for (const doc of workspace.textDocuments) {
        void maybeRetagCss(doc);
    }

    await server.start();
    server.show();
}

async function maybeRetagCss(doc: TextDocument): Promise<void> {
    if (doc.languageId !== "css" || doc.uri.scheme !== "file") {
        return;
    }
    if (isMarkupDir(path.dirname(doc.uri.fsPath))) {
        try {
            await languages.setTextDocumentLanguage(doc, "lumen-css");
        } catch {
            // Language not yet registered / doc closing - ignore.
        }
    }
}

export function deactivate(): Thenable<void> | undefined {
    return server?.stop();
}
