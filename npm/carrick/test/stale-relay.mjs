// A relay in front of a real language server that can serve one stale answer:
// `node stale-relay.mjs <trigger> <command> [args...]`. Everything passes
// through, and the last full diagnostic report for each file is kept. When the
// trigger file exists at a diagnostic pull, it is removed and the next pull of
// every file with a kept report is answered with that report, not passed on
// (what the language server did after an idle spell).
import { spawn } from "node:child_process";
import { existsSync, unlinkSync } from "node:fs";
import { Framer, frame } from "../src/lsp-frame.ts";

const [trigger, command, ...args] = process.argv.slice(2);
const server = spawn(command, args, { stdio: ["pipe", "pipe", "inherit"] });
const last = new Map();
const asked = new Map();
let stale = new Set();

const fromClient = new Framer();
process.stdin.on("data", (chunk) => {
  for (const m of fromClient.push(chunk)) {
    if (m.method === "textDocument/diagnostic") {
      const uri = m.params.textDocument.uri;
      if (existsSync(trigger)) {
        unlinkSync(trigger);
        stale = new Set(last.keys());
      }
      if (stale.delete(uri)) {
        process.stdout.write(frame({ jsonrpc: "2.0", id: m.id, result: last.get(uri) }));
        continue;
      }
      asked.set(m.id, uri);
    }
    server.stdin.write(frame(m));
  }
});
process.stdin.on("end", () => server.stdin.end());

const fromServer = new Framer();
server.stdout.on("data", (chunk) => {
  for (const m of fromServer.push(chunk)) {
    if (m.method === undefined && asked.has(m.id)) {
      const uri = asked.get(m.id);
      asked.delete(m.id);
      if (m.result?.kind === "full") last.set(uri, m.result);
    }
    process.stdout.write(frame(m));
  }
});
server.on("exit", (code) => process.exit(code ?? 0));
