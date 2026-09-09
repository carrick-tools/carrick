#!/usr/bin/env node
// Does the language server answer for this workspace?
//
// Every editor row in plugin/TEST-PLAN.md starts with the same by-hand
// sequence: drive `carrick lsp --stdio` the way a client does (`initialize`
// with the workspace folder, `initialized`, `didOpen`) and read the
// `publishDiagnostics` notifications. This is that sequence, with an expected
// output.
//
// What it separates: "the server answers for this workspace" from "this editor
// renders it". An editor that shows nothing is then diagnosed in one run rather
// than by reading two logs.
//
//   node scripts/lsp-probe.mjs --workspace <dir> --open <file> [--open <file>]
//   node scripts/lsp-probe.mjs --workspace <dir> --open <file> \
//     --server <install>/node_modules/carrick/dist/server.js
//   node scripts/lsp-probe.mjs --workspace <dir> --open <file> --json
//
// `--server` drives an installed package rather than this checkout, which makes
// it a post-install check as well as a development one. The default is the
// checkout's `src/server.ts`.
//
// The server's own log lines go to stderr as they arrive, prefixed, so the root
// it chose is observable in the same run — the root guard has no other
// observable effect.
//
// Non-zero exit when the server never started, when it published nothing at
// all, or when a `relatedInformation` location names a file that is not on
// disk. Everything else is reported, not judged: an empty diagnostic list for a
// file is a real answer.
//
// The last of those three is a regression guard rather than a live failure:
// `diagnostics.ts` already drops a counterpart whose file is not there, because
// a location an editor cannot open is a dead link in the Problems list. This is
// what notices if it stops.
//
// It reads the same `carrick` the server would (`CARRICK_BIN`, else `carrick`
// on PATH), so a workspace with no index answers "no answer" rather than
// failing here.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { LspClient } from "../test/lsp-client.ts";

const SEVERITY = { 1: "error", 2: "warning", 3: "information", 4: "hint" };

function usage(message) {
  process.stderr.write(
    `${message}\n\n` +
      "usage: lsp-probe.mjs --workspace <dir> --open <file> [--open <file>]\n" +
      "                     [--server <path>] [--json] [--timeout <ms>]\n",
  );
  process.exit(2);
}

function parseArgs(argv) {
  const options = { workspace: null, open: [], server: null, json: false, timeout: 15000 };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    const value = argv[i + 1];
    switch (arg) {
      case "--workspace":
        if (!value) usage("--workspace needs a directory");
        options.workspace = path.resolve(value);
        i++;
        break;
      case "--open":
        if (!value) usage("--open needs a file");
        options.open.push(path.resolve(value));
        i++;
        break;
      case "--server":
        if (!value) usage("--server needs a path");
        options.server = path.resolve(value);
        i++;
        break;
      case "--timeout":
        if (!value) usage("--timeout needs milliseconds");
        options.timeout = Number.parseInt(value, 10);
        i++;
        break;
      case "--json":
        options.json = true;
        break;
      default:
        usage(`unknown argument ${arg}`);
    }
  }
  if (!options.workspace) usage("--workspace is required");
  if (!options.open.length) usage("at least one --open is required");
  if (!Number.isFinite(options.timeout) || options.timeout <= 0) {
    usage("--timeout must be a positive number of milliseconds");
  }
  return options;
}

/** The path a `file:` URI names, or null when it is not one. */
function pathOfUri(uri) {
  try {
    return fileURLToPath(uri);
  } catch {
    return null;
  }
}

function realpath(file) {
  try {
    return fs.realpathSync(file);
  } catch {
    return file;
  }
}

/**
 * A file's path relative to the workspace, or its absolute path when it is
 * genuinely outside it.
 *
 * Both sides are resolved first: on macOS a workspace under `/var` is reported
 * by the server as `/private/var`, and a plain `path.relative` of the two turns
 * a sibling service into six `../` segments.
 */
function labelFor(file, workspace) {
  const resolved = realpath(file);
  for (const base of [workspace, realpath(workspace)]) {
    const relative = path.relative(base, resolved);
    if (relative && !relative.startsWith("..") && !path.isAbsolute(relative)) return relative;
  }
  return file;
}

/**
 * One row per published URI: the file, the diagnostic count, the severities,
 * the codes, and what its `relatedInformation` locations point at.
 *
 * The related check is the one that can fail. A counterpart location is a path
 * in another repo, and it only resolves when that repo is on disk — a
 * diagnostic that names a file nobody has is a diagnostic an editor renders as
 * a dead link.
 */
function row(publish, workspace) {
  const file = pathOfUri(publish.uri);
  const label = file ? labelFor(file, workspace) : publish.uri;
  const severities = new Map();
  const codes = new Set();
  const related = [];
  for (const diagnostic of publish.diagnostics) {
    const name = SEVERITY[diagnostic.severity] ?? `severity ${diagnostic.severity}`;
    severities.set(name, (severities.get(name) ?? 0) + 1);
    if (diagnostic.code !== undefined) codes.add(String(diagnostic.code));
    for (const item of diagnostic.relatedInformation ?? []) {
      const target = pathOfUri(item.location?.uri ?? "");
      related.push({ uri: item.location?.uri ?? null, path: target, exists: !!target && fs.existsSync(target) });
    }
  }
  return { uri: publish.uri, file, label, count: publish.diagnostics.length, severities, codes, related };
}

function print(rows, workspace) {
  process.stdout.write(`workspace ${workspace}\n`);
  for (const entry of rows) {
    const severities = [...entry.severities].map(([name, n]) => `${name}:${n}`).join(" ") || "-";
    const codes = [...entry.codes].join(",") || "-";
    const missing = entry.related.filter((item) => !item.exists);
    const relatedText = entry.related.length
      ? missing.length
        ? `related: ${entry.related.length}, ${missing.length} NOT on disk (${missing
            .map((item) => item.path ?? item.uri)
            .join(", ")})`
        : `related: ${entry.related.length}, all on disk`
      : "related: none";
    const count = `${entry.count} diagnostic${entry.count === 1 ? "" : "s"}`;
    process.stdout.write(
      `${entry.label}  ${count}  ${severities}  codes: ${codes}  ${relatedText}\n`,
    );
  }
}

const options = parseArgs(process.argv.slice(2));
if (!fs.existsSync(options.workspace)) usage(`no workspace at ${options.workspace}`);
for (const file of options.open) {
  if (!fs.existsSync(file)) usage(`no file at ${file}`);
}
if (options.server && !fs.existsSync(options.server)) usage(`no server at ${options.server}`);

const client = new LspClient({
  server: options.server ?? undefined,
  onStderr: (text) => {
    for (const line of text.split("\n")) {
      if (line.trim()) process.stderr.write(`[server] ${line}\n`);
    }
  },
});

let failure = null;
try {
  await client.initialize(options.workspace, "carrick lsp-probe");
  for (const file of options.open) client.open(file);
  try {
    await client.waitFor(() => client.publishes.length > 0, "a first publish", options.timeout);
  } catch {
    failure =
      client.exited !== null
        ? `the server exited (code ${client.exited.code}) before publishing anything`
        : `the server published nothing within ${options.timeout} ms`;
  }
  if (!failure) {
    const quiet = await client.settleUntilQuiet(700, options.timeout);
    if (!quiet) process.stderr.write(`[probe] still publishing at ${options.timeout} ms\n`);
  }
} catch (error) {
  failure = `the server never started: ${String(error)}`;
}

const rows = client.publishes.map((publish) => row(publish, options.workspace));
if (options.json) {
  process.stdout.write(`${JSON.stringify({ workspace: options.workspace, publishes: client.publishes }, null, 2)}\n`);
} else {
  print(rows, options.workspace);
}

const dangling = rows.flatMap((entry) => entry.related.filter((item) => !item.exists));
if (!failure && dangling.length) {
  failure = `${dangling.length} relatedInformation location(s) name a file that is not on disk`;
}

client.stop();

if (failure) {
  process.stderr.write(`FAIL ${failure}\n`);
  process.exit(1);
}
process.stdout.write(`OK ${rows.length} file(s) published\n`);
