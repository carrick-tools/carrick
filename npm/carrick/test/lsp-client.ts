// A minimal LSP client for driving the server over stdio.
//
// It speaks the same framing the server does, records every
// `textDocument/publishDiagnostics` notification, and keeps stderr so a test
// can assert on a log line (the root guard has no other observable effect).
//
// Two things here exist for `scripts/lsp-probe.mjs` rather than for a test:
// `server`, which points the client at an installed `dist/server.js` instead of
// this checkout's `src/server.ts`, and `onStderr`, which hands the server's log
// lines over as they arrive rather than at the end. A probe's whole output is
// what the server said and when.

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { pluginDir } from "./helpers.ts";

export type Publish = { uri: string; diagnostics: unknown[] };

export class LspClient {
  private readonly child: ChildProcessWithoutNullStreams;
  private buffer = Buffer.alloc(0);
  private nextId = 1;
  /** Milliseconds since the last publish arrived, for settling. */
  private lastPublishAt = 0;
  readonly publishes: Publish[] = [];
  readonly responses = new Map<number, unknown>();
  stderr = "";
  exited: { code: number | null; signal: NodeJS.Signals | null } | null = null;

  constructor(options: {
    args?: string[];
    env?: NodeJS.ProcessEnv;
    /** The server entry to run. Defaults to this checkout's `src/server.ts`. */
    server?: string;
    onStderr?: (chunk: string) => void;
  }) {
    const server = options.server ?? path.join(pluginDir, "src", "server.ts");
    this.child = spawn(process.execPath, [server, "--stdio", ...(options.args ?? [])], {
      env: options.env ?? process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });
    this.child.stdout.on("data", (chunk: Buffer) => this.consume(chunk));
    this.child.stderr.on("data", (chunk: Buffer) => {
      const text = chunk.toString("utf8");
      this.stderr += text;
      options.onStderr?.(text);
    });
    this.child.on("exit", (code, signal) => {
      this.exited = { code, signal };
    });
  }

  private consume(chunk: Buffer): void {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    for (;;) {
      const separator = this.buffer.indexOf("\r\n\r\n");
      if (separator === -1) return;
      const match = /Content-Length:\s*(\d+)/i.exec(
        this.buffer.subarray(0, separator).toString("ascii"),
      );
      if (!match?.[1]) {
        this.buffer = this.buffer.subarray(separator + 4);
        continue;
      }
      const length = Number.parseInt(match[1], 10);
      if (this.buffer.length < separator + 4 + length) return;
      const body = this.buffer.subarray(separator + 4, separator + 4 + length).toString("utf8");
      this.buffer = this.buffer.subarray(separator + 4 + length);
      const message = JSON.parse(body) as {
        id?: number;
        method?: string;
        params?: Publish;
        result?: unknown;
      };
      if (message.method === "textDocument/publishDiagnostics" && message.params) {
        this.publishes.push(message.params);
        this.lastPublishAt = Date.now();
      } else if (typeof message.id === "number") {
        this.responses.set(message.id, message.result);
      }
    }
  }

  private write(message: Record<string, unknown>): void {
    const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...message }), "utf8");
    this.child.stdin.write(`Content-Length: ${body.length}\r\n\r\n`);
    this.child.stdin.write(body);
  }

  notify(method: string, params: Record<string, unknown>): void {
    this.write({ method, params });
  }

  request(method: string, params: Record<string, unknown>): number {
    const id = this.nextId++;
    this.write({ id, method, params });
    return id;
  }

  async initialize(rootDir: string, clientName = "Claude Code"): Promise<void> {
    const uri = pathToFileURL(rootDir).toString();
    const id = this.request("initialize", {
      processId: process.pid,
      clientInfo: { name: clientName, version: "test" },
      workspaceFolders: [{ uri, name: path.basename(rootDir) }],
      rootUri: uri,
      rootPath: rootDir,
      capabilities: {},
    });
    await this.waitFor(() => this.responses.has(id), "initialize response");
    this.notify("initialized", {});
  }

  open(file: string): void {
    this.notify("textDocument/didOpen", {
      textDocument: {
        uri: pathToFileURL(file).toString(),
        languageId: "typescript",
        version: 1,
        text: "",
      },
    });
  }

  change(file: string, version: number): void {
    this.notify("textDocument/didChange", {
      textDocument: { uri: pathToFileURL(file).toString(), version },
      contentChanges: [{ text: "" }],
    });
  }

  async waitFor(predicate: () => boolean, what: string, timeoutMs = 5000): Promise<void> {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      if (predicate()) return;
      if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
  }

  /** Let anything the server was going to send arrive, then move on. */
  async settle(ms = 900): Promise<void> {
    await new Promise((resolve) => setTimeout(resolve, ms));
  }

  /**
   * Wait until the publishes stop arriving, rather than for a fixed time.
   *
   * A fixed wait has to be either long enough for the slowest workspace or
   * short enough to be quick, and a probe wants both: it returns as soon as
   * `quietMs` has passed with nothing new, and gives up at `capMs` whether the
   * server has finished or not. Answers false when the cap was reached, which
   * is a real observation about the workspace and not an error here.
   */
  async settleUntilQuiet(quietMs = 700, capMs = 20000): Promise<boolean> {
    const deadline = Date.now() + capMs;
    this.lastPublishAt = this.lastPublishAt || Date.now();
    for (;;) {
      if (Date.now() - this.lastPublishAt >= quietMs) return true;
      if (Date.now() > deadline) return false;
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
  }

  stop(): void {
    this.child.kill();
  }
}
