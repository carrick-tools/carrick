// The VS Code wrapper: a LanguageClient on `carrick lsp --stdio`, and one
// status bar item.
//
// Nearly everything the user sees is a diagnostic in the Problems panel, which
// is what an editor-hosted agent (Copilot agent mode, Cursor, Windsurf, Cline,
// Roo) reads after its own edits. An editor accepts any number of diagnostic
// providers per file, so this one sits beside TypeScript's rather than
// replacing it.
//
// The exception is the boundary, and it is the reason this file has any UI at
// all (carrick#879). The boundary says what the scan could not classify, so an
// empty Problems panel has an explanation; but it is one sentence about a
// service, not a finding about a file, and as a row on every TypeScript file a
// person opens it is the thing they would mute. So this extension tells the
// server it has somewhere else to put it (`boundarySurface`), and puts it
// there: a status bar item whose tooltip is the boundary. The server keeps
// stating the boundary in the agent's own channel either way.
//
// The other surface is the code lens (carrick#880), on the rows the index holds
// a counterpart or a mismatch for and on no others. Clicking one lists the
// other side and opens it, which is the one jump nothing else in the editor can
// make. The lens carries the boundary in its arguments rather than in a
// tooltip, because `vscode-languageclient` copies a command's title, name and
// arguments and drops the rest.
//
// Beyond those two the extension holds no code of its own: the server, the hook
// and the scanner are all the `carrick` npm package (carrick#710). That is the
// whole reason the extension has no version of the server to fall out of date
// with.

import * as vscode from "vscode";
import {
  LanguageClient,
  TransportKind,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let status: vscode.StatusBarItem | undefined;

/** What the server was last told about, so a boundary can be re-rendered. */
type BoundaryNotice = {
  service: string | null;
  indexCommit: string | null;
  lines: string[];
};

let lastBoundary: BoundaryNotice | undefined;

/**
 * The switches, read from settings, in the shape the server reads.
 *
 * `boundarySurface` is not a setting: it is this extension stating that it owns
 * a status bar, which is what moves the boundary off the Problems list.
 */
function surfaces(): Record<string, boolean> {
  const settings = vscode.workspace.getConfiguration("carrick");
  return {
    diagnostics: settings.get<boolean>("diagnostics") ?? true,
    definition: settings.get<boolean>("definition") ?? true,
    boundary: settings.get<boolean>("boundary") ?? true,
    codeLens: settings.get<boolean>("codeLens") ?? true,
    boundarySurface: true,
  };
}

/** One site on the other side of a row, as the lens command hands it over. */
type CounterpartSite = {
  role: string;
  service: string | null;
  /** Absolute, and already checked to exist; null when it is not on this disk. */
  path: string | null;
  line: number | null;
};

type CounterpartList = {
  operation: string;
  sites: CounterpartSite[];
  boundary: string[];
};

/**
 * What a lens does when it is clicked: list the other side, and open one.
 *
 * A site the server could not point at on this disk is shown and not opened:
 * saying "order-service, not on this machine" is the true answer, and a guessed
 * path would be a worse one. The boundary rides along as the placeholder,
 * because `vscode-languageclient` drops a command tooltip and this is the only
 * place a lens can carry it.
 */
async function showCounterparts(list: CounterpartList): Promise<void> {
  const items = list.sites.map((site) => ({
    label: site.service ? `${site.role} in ${site.service}` : site.role,
    description: site.path ? `${site.path}${site.line ? `:${site.line}` : ""}` : "not on this machine",
    site,
  }));
  const picked = await vscode.window.showQuickPick(items, {
    title: list.operation,
    placeHolder: list.boundary[0] ?? "The other side of this operation",
  });
  const site = picked?.site;
  if (!site?.path) return;
  const document = await vscode.workspace.openTextDocument(site.path);
  const line = Math.max(0, (site.line ?? 1) - 1);
  await vscode.window.showTextDocument(document, {
    selection: new vscode.Range(line, 0, line, 0),
  });
}

function renderStatus(): void {
  if (!status) return;
  const wanted = vscode.workspace.getConfiguration("carrick").get<boolean>("boundary") ?? true;
  if (!wanted || !lastBoundary) {
    status.hide();
    return;
  }
  const service = lastBoundary.service ?? "this workspace";
  const commit = lastBoundary.indexCommit ? ` ${lastBoundary.indexCommit.slice(0, 7)}` : "";
  status.text = `$(link) ${service}${commit}`;
  // The CLI's own sentences, as they arrived. Nothing is reworded here.
  status.tooltip = lastBoundary.lines.join("\n");
  status.show();
}

export function activate(context: vscode.ExtensionContext): void {
  const output = vscode.window.createOutputChannel("Carrick");
  context.subscriptions.push(output);

  status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 100);
  status.name = "Carrick";
  context.subscriptions.push(status);

  context.subscriptions.push(
    vscode.commands.registerCommand("carrick.showCounterparts", showCounterparts),
  );

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
    initializationOptions: surfaces(),
  };

  output.appendLine(
    `Starting ${command} lsp --stdio. If that command is not on PATH, install it with 'npm install -g carrick' or set carrick.binary to its full path.`,
  );
  client = new LanguageClient("carrick", "Carrick", serverOptions, clientOptions);
  context.subscriptions.push({ dispose: () => void client?.stop() });

  void client.start().then(() => {
    const listener = client?.onNotification("carrick/boundary", (notice: BoundaryNotice) => {
      lastBoundary = notice;
      renderStatus();
    });
    if (listener) context.subscriptions.push(listener);
  });

  // A setting changed is forwarded straight through, so a surface turned off
  // loses its rows on the next publish rather than at the next window reload.
  context.subscriptions.push(
    vscode.workspace.onDidChangeConfiguration((event) => {
      if (!event.affectsConfiguration("carrick")) return;
      renderStatus();
      void client?.sendNotification("workspace/didChangeConfiguration", {
        settings: { carrick: surfaces() },
      });
    }),
  );
}

export function deactivate(): Thenable<void> | undefined {
  return client?.stop();
}
