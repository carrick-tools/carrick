// The VS Code wrapper: a LanguageClient on the same stdio server, and no UI.
//
// Everything the user sees is a diagnostic in the Problems panel, which is what
// an editor-hosted agent (Copilot agent mode, Cursor, Windsurf, Cline, Roo)
// reads after its own edits. An editor accepts any number of diagnostic
// providers per file, so this one sits beside TypeScript's rather than
// replacing it.
//
// The extension holds no logic of its own. When the carrick npm package ships
// (carrick#710) the server command becomes `carrick lsp --stdio` and this file
// loses its path resolution.

import * as fs from "node:fs";
import * as path from "node:path";
import * as vscode from "vscode";
import {
  LanguageClient,
  TransportKind,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

/** Where the server lives: the setting, the packaged copy, then the repo. */
function serverEntry(context: vscode.ExtensionContext): string | null {
  const configured = vscode.workspace.getConfiguration("carrick").get<string>("serverPath");
  if (configured) return configured;
  const candidates = [
    path.join(context.extensionPath, "server", "server.ts"),
    path.join(context.extensionPath, "..", "src", "server.ts"),
  ];
  return candidates.find((candidate) => fs.existsSync(candidate)) ?? null;
}

export function activate(context: vscode.ExtensionContext): void {
  const entry = serverEntry(context);
  const output = vscode.window.createOutputChannel("Carrick");
  context.subscriptions.push(output);
  if (!entry) {
    output.appendLine("No Carrick language server found. Set carrick.serverPath to its entry point.");
    return;
  }

  const settings = vscode.workspace.getConfiguration("carrick");
  const env = { ...process.env };
  const binary = settings.get<string>("binary");
  if (binary) env["CARRICK_BIN"] = binary;

  const run = {
    command: settings.get<string>("nodePath") || "node",
    args: [entry, "--stdio"],
    transport: TransportKind.stdio,
    options: { env },
  };
  const serverOptions: ServerOptions = { run, debug: run };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "typescript" },
      { scheme: "file", language: "typescriptreact" },
    ],
    outputChannel: output,
  };

  client = new LanguageClient("carrick", "Carrick", serverOptions, clientOptions);
  context.subscriptions.push({ dispose: () => void client?.stop() });
  void client.start();
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
