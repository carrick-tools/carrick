// The VS Code wrapper: a LanguageClient on `carrick lsp --stdio`, and no UI.
//
// Everything the user sees is a diagnostic in the Problems panel, which is what
// an editor-hosted agent (Copilot agent mode, Cursor, Windsurf, Cline, Roo)
// reads after its own edits. An editor accepts any number of diagnostic
// providers per file, so this one sits beside TypeScript's rather than
// replacing it.
//
// The extension holds no code of its own: the server, the hook and the scanner
// are all the `carrick` npm package (carrick#710), and this file only starts
// it. That is the whole reason the extension has no version of the server to
// fall out of date with.

import * as vscode from "vscode";
import {
  LanguageClient,
  TransportKind,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("Carrick");
  context.subscriptions.push(output);

  const settings = vscode.workspace.getConfiguration("carrick");
  const command = settings.get<string>("binary") || "carrick";
  const run = {
    command,
    args: ["lsp", "--stdio"],
    transport: TransportKind.stdio,
  };
  const serverOptions: ServerOptions = { run, debug: run };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [
      { scheme: "file", language: "typescript" },
      { scheme: "file", language: "typescriptreact" },
    ],
    outputChannel: output,
  };

  output.appendLine(
    `Starting ${command} lsp --stdio. If that command is not on PATH, install it with 'npm install -g carrick' or set carrick.binary to its full path.`,
  );
  client = new LanguageClient("carrick", "Carrick", serverOptions, clientOptions);
  context.subscriptions.push({ dispose: () => void client?.stop() });
  void client.start();
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
