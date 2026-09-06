#!/usr/bin/env node
// The Carrick language server: stdio, client-agnostic, read-only.
//
// It answers one question for every file a client opens or changes: what does
// the workspace index say about the routes and calls in this file, and does
// anything on the other side of them disagree. The answer comes from
// `carrick check <file> --json`; nothing is computed here.
//
// Client differences it is built for (verified on the 2026-09-05 spike):
//
// * Claude Code sends `didOpen` only, with the post-edit text, and delivers
//   diagnostics into the model one turn later. Editors send `didChange` too, so
//   both are handled and `didChange` is debounced.
// * Claude Code runs one server per file extension: with a TypeScript server
//   enabled, this one is not started at all and the PostToolUse hook is the
//   channel. Editors accept any number of diagnostic providers per file.
// * Claude Code drops `relatedInformation`; editors render it. Counterpart
//   sites go in both places (see diagnostics.ts).
//
// stdout is protocol only. Every log line goes to stderr.

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { check } from "./cli.ts";
import { resolveChannel, type ChannelChoice } from "./channel.ts";
import { toDiagnostics, type Diagnostic } from "./diagnostics.ts";
import { createLogger } from "./log.ts";
import { resolveRoot, rootNote, type RootChoice } from "./root.ts";

const NAME = "carrick";
const VERSION = "0.0.1";
/** Editors fire `didChange` on every keystroke; one check per pause is enough. */
const DEBOUNCE_MS = 400;

const log = createLogger("carrick-lsp");

type Message = {
  jsonrpc?: string;
  id?: number | string;
  method?: string;
  params?: Record<string, unknown>;
};

function send(message: Record<string, unknown>): void {
  const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", ...message }), "utf8");
  process.stdout.write(`Content-Length: ${body.length}\r\n\r\n`);
  process.stdout.write(body);
}

// ------------------------------------------------------------------ state

let root = process.cwd();
let rootChoice: RootChoice | null = null;
let channel: ChannelChoice = resolveChannel({ hooksInstalled: false });
let clientName = "an unnamed client";

/** What each checked file last published to, so a clear is scoped to it. */
const publishedBy = new Map<string, Set<string>>();
const debounces = new Map<string, NodeJS.Timeout>();
/** One check at a time: a burst of edits must not interleave two runs. */
let queue: Promise<void> = Promise.resolve();

function enqueue(work: () => Promise<void>): Promise<void> {
  queue = queue.then(work).catch((error) => log("queue error", String(error)));
  return queue;
}

function relative(file: string): string {
  return path.isAbsolute(file) ? path.relative(root, file) : file;
}

function publish(uriPath: string, diagnostics: Diagnostic[]): void {
  send({
    method: "textDocument/publishDiagnostics",
    params: { uri: pathToFileURL(uriPath).toString(), diagnostics },
  });
}

async function checkAndPublish(absFile: string): Promise<void> {
  const relFile = relative(absFile);
  const outcome = await check(relFile, { cwd: root });
  if (!outcome.result) {
    log("no answer for", relFile, outcome.failure ?? "");
    return;
  }
  if (outcome.result.error) {
    log("check", relFile, "->", outcome.result.error);
    return;
  }
  const byFile = toDiagnostics(outcome.result, root, relFile);
  log(
    `check ${relFile} -> ${[...byFile.values()].reduce((n, list) => n + list.length, 0)} diagnostic(s) across ${byFile.size} file(s) in ${outcome.ms}ms`,
  );

  // Clear only what THIS file's previous check flagged and no longer does: a
  // check of another file legitimately returns nothing while its findings stand.
  const previous = publishedBy.get(relFile) ?? new Set<string>();
  for (const stale of previous) if (!byFile.has(stale)) byFile.set(stale, []);
  publishedBy.set(
    relFile,
    new Set([...byFile.keys()].filter((file) => (byFile.get(file) ?? []).length > 0)),
  );
  for (const [file, diagnostics] of byFile) publish(file, diagnostics);
}

function scheduleCheck(absFile: string, debounceMs: number): void {
  const existing = debounces.get(absFile);
  if (existing) clearTimeout(existing);
  if (debounceMs === 0) {
    void enqueue(() => checkAndPublish(absFile));
    return;
  }
  debounces.set(
    absFile,
    setTimeout(() => {
      debounces.delete(absFile);
      void enqueue(() => checkAndPublish(absFile));
    }, debounceMs),
  );
}

function pathOf(params: Record<string, unknown> | undefined): string | null {
  const document = params?.["textDocument"] as { uri?: string } | undefined;
  if (!document?.uri) return null;
  try {
    return fileURLToPath(document.uri);
  } catch {
    return null;
  }
}

// ------------------------------------------------------------------ protocol

function onInitialize(id: number | string | undefined, params: Record<string, unknown>): void {
  const folders = params["workspaceFolders"] as Array<{ uri?: string }> | undefined;
  const folderUri = folders?.[0]?.uri ?? (params["rootUri"] as string | undefined);
  let clientRoot: string | null = null;
  if (folderUri) {
    try {
      clientRoot = fileURLToPath(folderUri);
    } catch {
      clientRoot = null;
    }
  }
  if (!clientRoot && typeof params["rootPath"] === "string") {
    clientRoot = params["rootPath"] as string;
  }
  const info = params["clientInfo"] as { name?: string } | undefined;
  if (info?.name) clientName = info.name;

  rootChoice = resolveRoot({
    clientRoot,
    projectDir: process.env["CLAUDE_PROJECT_DIR"] ?? null,
    filePath: clientRoot,
  });
  root = rootChoice.root;
  const note = rootNote(rootChoice);
  if (note) log(note);

  channel = resolveChannel({
    hooksInstalled: process.argv.includes("--hooks-installed"),
  });
  if (channel.ignoredValue) {
    log(`CARRICK_CHANNEL=${channel.ignoredValue} is not a channel; using ${channel.channel}`);
  }
  if (channel.channel !== "lsp") {
    log(
      `the ${channel.channel} channel owns delivery in this install (${channel.source}), so this server publishes nothing`,
    );
  }
  log(`initialized by ${clientName}, root ${root} (${rootChoice.source})`);

  send({
    id,
    result: {
      capabilities: {
        textDocumentSync: { openClose: true, change: 1, save: { includeText: false } },
        diagnosticProvider: {
          identifier: NAME,
          interFileDependencies: true,
          workspaceDiagnostics: false,
        },
      },
      serverInfo: { name: NAME, version: VERSION },
    },
  });
}

function handle(message: Message): void {
  const { id, method, params } = message;
  if (!method) return;
  switch (method) {
    case "initialize":
      onInitialize(id, params ?? {});
      return;
    case "initialized":
      return;
    case "shutdown":
      send({ id, result: null });
      return;
    case "exit":
      process.exit(0);
      return;
    case "textDocument/didOpen":
    case "textDocument/didSave":
    case "textDocument/didChange": {
      if (channel.channel !== "lsp") return;
      const file = pathOf(params);
      if (!file) return;
      scheduleCheck(file, method === "textDocument/didChange" ? DEBOUNCE_MS : 0);
      return;
    }
    case "textDocument/diagnostic": {
      // A pull-only client still gets an answer, and the same one.
      const file = pathOf(params);
      if (!file || channel.channel !== "lsp") {
        send({ id, result: { kind: "full", items: [] } });
        return;
      }
      void enqueue(async () => {
        const outcome = await check(relative(file), { cwd: root });
        const items = outcome.result
          ? (toDiagnostics(outcome.result, root, relative(file)).get(path.resolve(file)) ?? [])
          : [];
        send({ id, result: { kind: "full", items } });
      });
      return;
    }
    default:
      if (id !== undefined) {
        send({ id, error: { code: -32601, message: `method not handled: ${method}` } });
      }
      return;
  }
}

// ------------------------------------------------------------------ framing

let buffer = Buffer.alloc(0);

export function feed(chunk: Buffer): void {
  buffer = Buffer.concat([buffer, chunk]);
  for (;;) {
    const separator = buffer.indexOf("\r\n\r\n");
    if (separator === -1) return;
    const header = buffer.subarray(0, separator).toString("ascii");
    const match = /Content-Length:\s*(\d+)/i.exec(header);
    if (!match?.[1]) {
      buffer = buffer.subarray(separator + 4);
      continue;
    }
    const length = Number.parseInt(match[1], 10);
    if (buffer.length < separator + 4 + length) return;
    const body = buffer.subarray(separator + 4, separator + 4 + length).toString("utf8");
    buffer = buffer.subarray(separator + 4 + length);
    let message: Message;
    try {
      message = JSON.parse(body) as Message;
    } catch (error) {
      log("unparseable message", String(error));
      continue;
    }
    try {
      handle(message);
    } catch (error) {
      log("handler error", String(error));
    }
  }
}

process.stdin.on("data", (chunk: Buffer) => feed(chunk));
process.on("uncaughtException", (error) => log("uncaught", String(error)));
process.on("unhandledRejection", (error) => log("unhandled rejection", String(error)));
log(`start pid ${process.pid} node ${process.version}`);
