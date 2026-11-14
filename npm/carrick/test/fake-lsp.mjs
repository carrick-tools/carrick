// A stand-in language server for the engine's failure paths: it answers every
// diagnostic pull with a report that is not "full".
import { Framer, frame } from "../src/lsp-frame.ts";

const framer = new Framer();
const out = (m) => process.stdout.write(frame(m));
process.stdin.on("data", (chunk) => {
  for (const m of framer.push(chunk)) {
    if (m.method === "initialize") out({ id: m.id, result: { capabilities: {} } });
    else if (m.method === "textDocument/diagnostic") out({ id: m.id, result: { kind: "unchanged", resultId: "1" } });
    else if (m.method === "shutdown") out({ id: m.id, result: null });
    else if (m.method === "exit") process.exit(0);
  }
});
