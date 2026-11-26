// A stand-in language server for the engine's failure paths.
//
// `node fake-lsp.mjs [mode] [log]`:
// - `unchanged` (the default): every diagnostic pull answers a report that is not "full".
// - `fresh`: every pull answers the report of the text it holds now.
// - `stale-once`: the second pull answers the first pull's report again (what a
//   language server did after an idle spell); every other pull is fresh.
// - `always-stale`: every pull after the first answers the first pull's report.
// With a log path, each `didOpen`/`didChange` version and each diagnostic pull is
// appended to it as one line.
//
// A report reads the text it holds: a line with `ERR` is an error there; a
// marker `const __carrick_rev: "rN" = "rN_";` is a type error naming `"rN_"`;
// an unclosed `{` is `'}' expected` at the end of the file; `// @ts-nocheck`
// silences everything.
import { appendFileSync } from "node:fs";
import { Framer, frame } from "../src/lsp-frame.ts";

const mode = process.argv[2] ?? "unchanged";
const logFile = process.argv[3];
const note = (line) => {
  if (logFile) appendFileSync(logFile, `${line}\n`);
};

const texts = new Map();
let pulls = 0;
let first = null;

function report(text) {
  if (text.includes("// @ts-nocheck")) return [];
  const lines = text.split("\n");
  const items = [];
  lines.forEach((line, i) => {
    const err = line.indexOf("ERR");
    if (err >= 0) items.push({ range: { start: { line: i, character: err }, end: { line: i, character: err + 3 } }, severity: 1, code: 9999, message: "fake error" });
    const mark = /const __carrick_rev: "r(\d+)" = "r\1_";/.exec(line);
    if (mark) {
      const at = line.indexOf("__carrick_rev");
      items.push({ range: { start: { line: i, character: at }, end: { line: i, character: at + 13 } }, severity: 1, code: 2322, message: `Type '"r${mark[1]}_"' is not assignable to type '"r${mark[1]}"'.` });
    }
  });
  const open = (text.match(/\{/g) ?? []).length - (text.match(/\}/g) ?? []).length;
  if (open > 0) {
    const last = lines.length - 1;
    const end = { line: last, character: lines[last].length };
    items.push({ range: { start: end, end }, severity: 1, code: 1005, message: "'}' expected." });
  }
  return items;
}

const framer = new Framer();
const out = (m) => process.stdout.write(frame(m));
process.stdin.on("data", (chunk) => {
  for (const m of framer.push(chunk)) {
    if (m.method === "initialize") out({ id: m.id, result: { capabilities: {} } });
    else if (m.method === "textDocument/didOpen") {
      texts.set(m.params.textDocument.uri, m.params.textDocument.text);
      note(`open ${m.params.textDocument.version}`);
      if (logFile) appendFileSync(`${logFile}.lang`, `${m.params.textDocument.uri.split("/").pop()} ${m.params.textDocument.languageId}\n`);
    } else if (m.method === "textDocument/didChange") {
      texts.set(m.params.textDocument.uri, m.params.contentChanges[0].text);
      note(`change ${m.params.textDocument.version}`);
    } else if (m.method === "textDocument/diagnostic") {
      note("pull");
      if (mode === "unchanged") {
        out({ id: m.id, result: { kind: "unchanged", resultId: "1" } });
        continue;
      }
      pulls++;
      const fresh = report(texts.get(m.params.textDocument.uri) ?? "");
      first ??= fresh;
      const stale = (mode === "stale-once" && pulls === 2) || (mode === "always-stale" && pulls > 1);
      out({ id: m.id, result: { kind: "full", items: stale ? first : fresh } });
    } else if (m.method === "shutdown") out({ id: m.id, result: null });
    else if (m.method === "exit") process.exit(0);
  }
});
