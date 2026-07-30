// LSP client lifecycle: discover + spawn `lumen-lsp`, wire the document
// selectors, drive a status-bar item, and support restart.
//
// Mirrors rust-analyzer's extension: a single managed client, a status-bar
// entry that reflects server health, and a "restart" command. Language
// intelligence (diagnostics/completion/hover/format/rename/symbols) lives in
// the Rust `lumen-lsp` server - this file only transports it.

import {
    ExtensionContext,
    StatusBarAlignment,
    StatusBarItem,
    window,
    workspace,
} from "vscode";
import {
    LanguageClient,
    LanguageClientOptions,
    ServerOptions,
    TransportKind,
} from "vscode-languageclient/node";
import { resolveBinary } from "./config";

export class LumenServer {
    private client: LanguageClient | undefined;
    private status: StatusBarItem;

    constructor(private readonly ctx: ExtensionContext) {
        this.status = window.createStatusBarItem(StatusBarAlignment.Left, 0);
        this.status.command = "lumen.restartServer";
        this.ctx.subscriptions.push(this.status);
    }

    /** (Re)start the server. Safe to call repeatedly. */
    async start(): Promise<void> {
        await this.stop();

        const { command, discovered } = resolveBinary("lumen-lsp", "serverPath");

        const serverOptions: ServerOptions = {
            run: { command, transport: TransportKind.stdio },
            debug: { command, transport: TransportKind.stdio },
        };

        const clientOptions: LanguageClientOptions = {
            documentSelector: [
                { scheme: "file", language: "lumen-markup" },
                { scheme: "file", language: "lumen-css" },
                { scheme: "file", language: "lumen-rhai" },
                // Route Lumen stylesheets kept under the built-in `css`
                // language to the server for stylesheet diagnostics too.
                { scheme: "file", language: "css", pattern: "**/*.css" },
            ],
            synchronize: {
                fileEvents: workspace.createFileSystemWatcher("**/*.{lmn,css,rhai}"),
            },
            outputChannelName: "Lumen LSP",
            traceOutputChannel: window.createOutputChannel("Lumen LSP Trace"),
        };

        this.client = new LanguageClient(
            "lumen",
            "Lumen",
            serverOptions,
            clientOptions,
        );

        // Trace verbosity is read automatically by the client from the
        // `lumen.trace.server` setting (its config section is the client id).
        this.setStatus("$(sync~spin) Lumen: starting", "Starting lumen-lsp...");
        try {
            await this.client.start();
            this.setStatus("$(lightbulb) Lumen", `lumen-lsp running (${command})`);
        } catch (err) {
            this.client = undefined;
            this.setStatus(
                "$(error) Lumen: no server",
                `lumen-lsp failed to start (tried '${command}'). Click to retry.`,
            );
            const hint = discovered
                ? `Tried '${command}'.`
                : `'${command}' was not on $PATH and no target/ build was found.`;
            window
                .showErrorMessage(
                    `Lumen language server failed to start. ${hint} ` +
                        `Build it with 'cargo build -p lumen-lsp' or set 'lumen.serverPath'. ` +
                        `Syntax highlighting still works; diagnostics/completion are disabled. (${err})`,
                    "Open Settings",
                )
                .then((choice) => {
                    if (choice === "Open Settings") {
                        void window.showInformationMessage(
                            "Set 'lumen.serverPath' to your built lumen-lsp binary.",
                        );
                    }
                });
        }
    }

    async stop(): Promise<void> {
        if (this.client) {
            const c = this.client;
            this.client = undefined;
            try {
                await c.stop();
            } catch {
                // Already down.
            }
        }
    }

    show(): void {
        this.status.show();
    }

    private setStatus(text: string, tooltip: string): void {
        this.status.text = text;
        this.status.tooltip = tooltip;
        this.status.show();
    }
}
