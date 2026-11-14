// The held compiler: one language server kept open over a shadow tree, so an
// edit is judged in hundreds of milliseconds instead of a cold compile.
//
// `Engine` is the seam: the daemon asks for diagnostics and never learns which
// compiler answers. `Ts7Lsp` is the TypeScript 7 implementation, spoken to over
// `tsc --lsp --stdio`. Another compiler swaps in behind the same interface.

import { execFileSync, spawn, type ChildProcessWithoutNullStreams } from "node:child_process";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { Framer, frame, type LspMessage } from "../lsp-frame.ts";

export type Diag = {
  range: {
    start: { line: number; character: number };
    end: { line: number; character: number };
  };
  severity?: number;
  code?: number | string;
  message: string;
};

export interface Engine {
  /** The file's whole text, as it is now. The first call opens it, later ones replace it. */
  set(file: string, text: string): void;
  /** Diagnostics for each file, asked together. Rejects when the compiler cannot answer in full. */
  pull(files: string[]): Promise<Map<string, Diag[]>>;
  /** Resident memory of the compiler process in MB, or null when it is not running. */
  rss(): number | null;
  close(): Promise<void>;
}

type Pending = { resolve: (message: LspMessage) => void; reject: (error: Error) => void };

export class Ts7Lsp implements Engine {
  private readonly child: ChildProcessWithoutNullStreams;
  private readonly framer = new Framer();
  private readonly pending = new Map<number, Pending>();
  private readonly versions = new Map<string, number>();
  private readonly cwd: string;
  private nextId = 1;
  private exited = false;
  /** Everything sent after `initialize` waits on this, so order is the call order. */
  private readonly ready: Promise<void>;
  stderr = "";

  /**
   * @param command  the native compiler executable (a `tsc` that speaks `--lsp`)
   * @param cwd      the shadow's `w` directory, where the generated tsconfig is
   */
  constructor(command: string, cwd: string, args: string[] = ["--lsp", "--stdio"]) {
    this.cwd = cwd;
    this.child = spawn(command, args, {
      cwd,
      stdio: ["pipe", "pipe", "pipe"],
      env: { ...process.env, DO_NOT_TRACK: "1" },
    });
    this.child.stdout.on("data", (chunk: Buffer) => this.consume(chunk));
    this.child.stderr.on("data", (chunk: Buffer) => {
      this.stderr += chunk.toString("utf8");
    });
    this.child.on("error", (error) => this.fail(error));
    this.child.on("exit", () => {
      this.exited = true;
      this.fail(new Error("compiler exited"));
    });
    this.ready = this.initialize();
    // A start that fails is reported by the first `pull`; it must not also be
    // an unhandled rejection here.
    this.ready.catch(() => undefined);
  }

  private fail(error: Error): void {
    for (const pending of this.pending.values()) pending.reject(error);
    this.pending.clear();
  }

  private consume(chunk: Buffer): void {
    for (const message of this.framer.push(chunk)) {
      if (message.id !== undefined && message.method) {
        // A request from the compiler: settings come back empty, anything else is accepted.
        const items = (message.params?.["items"] as unknown[] | undefined) ?? [];
        const result = message.method === "workspace/configuration" ? items.map(() => null) : null;
        this.write({ id: message.id, result });
      } else if (typeof message.id === "number") {
        const pending = this.pending.get(message.id);
        this.pending.delete(message.id);
        pending?.resolve(message);
      }
    }
  }

  private write(message: Record<string, unknown>): void {
    if (this.exited) return;
    this.child.stdin.write(frame(message));
  }

  private request(method: string, params: Record<string, unknown> | null): Promise<LspMessage> {
    return new Promise((resolve, reject) => {
      if (this.exited) {
        reject(new Error("compiler exited"));
        return;
      }
      const id = this.nextId++;
      this.pending.set(id, { resolve, reject });
      this.write({ id, method, params });
    });
  }

  private async initialize(): Promise<void> {
    const uri = pathToFileURL(this.cwd).toString();
    const answer = await this.request("initialize", {
      processId: process.pid,
      rootUri: uri,
      workspaceFolders: [{ uri, name: path.basename(path.dirname(this.cwd)) }],
      capabilities: {
        textDocument: {
          diagnostic: { dynamicRegistration: false },
          synchronization: { didSave: true },
        },
        workspace: { configuration: true, workspaceFolders: true },
      },
    });
    if (answer.error) throw new Error(`initialize failed: ${answer.error.message}`);
    this.write({ method: "initialized", params: {} });
  }

  set(file: string, text: string): void {
    const uri = pathToFileURL(file).toString();
    const version = (this.versions.get(file) ?? 0) + 1;
    this.versions.set(file, version);
    const message =
      version === 1
        ? {
            method: "textDocument/didOpen",
            params: { textDocument: { uri, languageId: "typescript", version, text } },
          }
        : {
            method: "textDocument/didChange",
            params: { textDocument: { uri, version }, contentChanges: [{ text }] },
          };
    void this.ready.then(() => this.write(message)).catch(() => undefined);
  }

  async pull(files: string[]): Promise<Map<string, Diag[]>> {
    await this.ready;
    const answers = await Promise.all(
      files.map((file) =>
        this.request("textDocument/diagnostic", {
          textDocument: { uri: pathToFileURL(file).toString() },
        }),
      ),
    );
    const out = new Map<string, Diag[]>();
    files.forEach((file, i) => {
      const answer = answers[i]!;
      if (answer.error) throw new Error(`diagnostic request failed: ${answer.error.message}`);
      const result = answer.result as { kind?: string; items?: Diag[] } | null;
      // Anything but a full report is not "no diagnostics": it is no answer.
      if (result?.kind !== "full") {
        throw new Error(`diagnostic answer for ${file} is not a full report (${result?.kind ?? "null"})`);
      }
      out.set(file, result.items ?? []);
    });
    return out;
  }

  rss(): number | null {
    const pid = this.child.pid;
    if (this.exited || pid === undefined) return null;
    try {
      const kb = Number.parseInt(execFileSync("ps", ["-o", "rss=", "-p", String(pid)]).toString().trim(), 10);
      return Number.isFinite(kb) ? Math.round(kb / 1024) : null;
    } catch {
      return null;
    }
  }

  async close(): Promise<void> {
    if (this.exited) return;
    const exit = new Promise<void>((resolve) => this.child.once("exit", () => resolve()));
    try {
      await Promise.race([
        this.ready.then(() => this.request("shutdown", null)),
        new Promise((resolve) => setTimeout(resolve, 2000)),
      ]);
      this.write({ method: "exit" });
    } catch {
      // Killed below.
    }
    const timer = setTimeout(() => this.child.kill("SIGKILL"), 3000);
    await exit;
    clearTimeout(timer);
  }
}
