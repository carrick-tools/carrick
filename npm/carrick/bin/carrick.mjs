#!/usr/bin/env node
// One command for every host.
//
// `carrick index|touch|check|refresh|status|<path>` is the Rust scanner, run
// from the platform package npm installed. `carrick lsp`, `carrick hook` and
// `carrick init` are this package's own TypeScript, run by Node's type
// stripping — which is why the floor is Node 24 and why the check for it is
// the first thing in this file, before any `.ts` is imported. On an older Node
// the import itself is a syntax error, and a syntax error is not an answer to
// "which Node do I need".
//
// What it imports is `dist/`, not `src/`: Node refuses to strip types for any
// file under node_modules, so the published package carries the emit and the
// checkout builds it (`npm run build`, and `prepack` before a publish).
//
// Nothing here re-enters `carrick` on PATH: the binary is resolved to an
// absolute path (src/native.ts). A shim that shelled out to its own name would
// loop forever the moment the platform package went missing.

import { spawn } from "node:child_process";

const NODE_FLOOR = 24;

function nodeMajor() {
  const [major] = process.versions.node.split(".");
  return Number.parseInt(major ?? "0", 10);
}

if (nodeMajor() < NODE_FLOOR) {
  process.stderr.write(
    `carrick needs Node ${NODE_FLOOR} or newer; this is Node ${process.versions.node}.\n` +
      `The type sidecar that resolves your request and response types runs on it, and this CLI is TypeScript run directly.\n` +
      `Install Node ${NODE_FLOOR}+ (https://nodejs.org) and run carrick again.\n`,
  );
  process.exit(1);
}

const HOOKS = {
  "post-edit": "../dist/hook/post-edit.js",
  "session-start": "../dist/hook/session-start.js",
};

const argv = process.argv.slice(2);
const [command, ...rest] = argv;

/**
 * Run a build of the index and render it (carrick#1315).
 *
 * The binary writes for a log file as well as for a person: a run banner, a
 * spinner per phase and a boundary census. Rendered, this holds its streams
 * and draws the markers it states through the renderer `carrick init` uses.
 * `--verbose` and `--detach` never reach here — they want the binary's own
 * output — and a run that draws nothing writes its output through untouched.
 */
async function renderNative(args) {
  const { resolveNativeBinary, nativeEnv, overrideLine } = await import("../dist/native.js");
  const { renderScan } = await import("../dist/scan.js");
  const lookup = resolveNativeBinary();
  if (!lookup.binary) {
    process.stderr.write(`carrick: ${lookup.problem}\n`);
    process.exit(1);
  }
  const override = overrideLine(lookup);
  if (override) process.stderr.write(`${override}\n`);
  const { readFile } = await import("node:fs/promises");
  const { version } = JSON.parse(
    await readFile(new URL("../package.json", import.meta.url), "utf8"),
  );
  let outcome;
  try {
    outcome = await renderScan({
      binary: lookup.binary,
      args,
      env: nativeEnv(),
      version,
    });
  } catch (error) {
    process.stderr.write(`carrick: could not run ${lookup.binary}: ${error.message}\n`);
    process.exit(1);
  }
  if (outcome.signal) {
    process.kill(process.pid, outcome.signal);
    return;
  }
  process.exit(outcome.code);
}

/** Run the scanner binary, streaming its output and answering with its code. */
async function runNative(args) {
  const { resolveNativeBinary, nativeEnv, overrideLine } = await import("../dist/native.js");
  const lookup = resolveNativeBinary();
  if (!lookup.binary) {
    process.stderr.write(`carrick: ${lookup.problem}\n`);
    process.exit(1);
  }
  // On stderr, so a `--json` answer on stdout stays parseable.
  const override = overrideLine(lookup);
  if (override) process.stderr.write(`${override}\n`);
  const child = spawn(lookup.binary, args, {
    stdio: "inherit",
    env: nativeEnv(),
  });
  child.on("error", (error) => {
    process.stderr.write(`carrick: could not run ${lookup.binary}: ${error.message}\n`);
    process.exit(1);
  });
  child.on("exit", (code, signal) => {
    // A signalled child is reported the way a shell reports it, so a Ctrl-C
    // through this shim looks like a Ctrl-C of the scanner.
    if (signal) {
      process.kill(process.pid, signal);
      return;
    }
    process.exit(code ?? 1);
  });
}

/**
 * Point this package's own commands at the binary directly, rather than at
 * whatever `carrick` PATH holds. The hook and the server run on every edit;
 * one process, not two, and no dependence on the install being global.
 */
async function pointAtNativeBinary() {
  const { resolveNativeBinary, resolveSidecarDir } = await import("../dist/native.js");
  if (!process.env["CARRICK_BIN"]) {
    const lookup = resolveNativeBinary();
    if (lookup.binary) process.env["CARRICK_BIN"] = lookup.binary;
  }
  if (!process.env["CARRICK_SIDECAR_DIR"]) {
    const sidecar = resolveSidecarDir();
    if (sidecar) process.env["CARRICK_SIDECAR_DIR"] = sidecar;
  }
}

async function printVersion() {
  const { readFile } = await import("node:fs/promises");
  const manifest = new URL("../package.json", import.meta.url);
  const { version } = JSON.parse(await readFile(manifest, "utf8"));
  process.stdout.write(`${version}\n`);
}

/**
 * The commands this package adds to the binary's own.
 *
 * Printed after the scanner's help rather than woven into it: the binary is
 * spawned for that text and answers none of these names, so its groups are
 * repeated here as headings instead of being interleaved (carrick#976).
 *
 * `init` is named in the binary's own WORKSPACE block instead of here: it is
 * where the first run starts, and a help text a reader stops part-way through
 * must not be the reason they never find it (carrick#997 item 5). Its
 * arguments are still this package's.
 */
function extraHelp() {
  return [
    "This package carries the commands that put those answers where you work.",
    "",
    "ACCOUNT:",
    "    login                        sign in to a Carrick workspace",
    "    logout                       remove the saved local credential",
    "",
    "INTEGRATION:",
    "    lsp --stdio                  the language server, for an editor or an agent",
    "                                 that speaks LSP",
    "    hook post-edit               Claude Code PostToolUse hook (reads the tool",
    "                                 payload on stdin)",
    "    hook session-start           Claude Code SessionStart hook",
    "    templates <name>             print a file to add to a repo: workflow, or",
    "                                 carrick.json",
    "    --version                    the version of this package",
    "",
    "`carrick init [--project SLUG]` sets this folder up; `carrick init --help`",
    "prints its own arguments. `carrick doctor` re-checks that setup and exits",
    "non-zero on anything it finds. `carrick remove` undoes what init wrote on",
    "this machine and lists the files the scaffold added to the repository.",
    "",
  ].join("\n");
}

switch (command) {
  case "login": {
    const { login } = await import("../dist/auth/run.js");
    process.exit(await login(rest));
  }
  case "logout": {
    const { logout } = await import("../dist/auth/run.js");
    process.exit(logout(rest));
  }
  case "lsp": {
    await pointAtNativeBinary();
    await import("../dist/server.js");
    break;
  }
  case "hook": {
    const [name] = rest;
    const entry = name ? HOOKS[name] : undefined;
    if (!entry) {
      process.stderr.write(
        `carrick hook needs one of: ${Object.keys(HOOKS).join(", ")}\n`,
      );
      process.exit(2);
    }
    await pointAtNativeBinary();
    await import(entry);
    break;
  }
  case "init": {
    const { init } = await import("../dist/init/run.js");
    process.exit(await init(rest));
  }
  // Named here rather than left to the binary: `remove` is this package's own
  // command, and the binary reads a name it does not know as a path to scan
  // (carrick#981).
  case "remove": {
    const { remove } = await import("../dist/init/remove.js");
    process.exit(await remove(rest));
  }
  // Also this package's own: the hook, MCP and workflow halves of the check
  // are things this package wrote, and the binary answers for the rest of it
  // through `carrick status`.
  case "doctor": {
    const { doctor } = await import("../dist/init/doctor.js");
    process.exit(await doctor(rest));
  }
  case "templates": {
    const { templates } = await import("../dist/init/run.js");
    process.exit(templates(rest));
  }
  case "--version":
  case "-V": {
    await printVersion();
    break;
  }
  case "--help":
  case "-h":
  case undefined: {
    // The scanner owns its own help, including the environment variables, so
    // print that and add what this package puts on top of it.
    const { resolveNativeBinary, nativeEnv } = await import("../dist/native.js");
    const lookup = resolveNativeBinary();
    if (lookup.binary) {
      await new Promise((resolve) => {
        spawn(lookup.binary, ["--help"], { stdio: "inherit", env: nativeEnv() }).on(
          "exit",
          resolve,
        );
      });
    } else {
      process.stderr.write(`carrick: ${lookup.problem}\n`);
    }
    process.stderr.write(extraHelp());
    process.exit(0);
  }
  default: {
    const { isRendered } = await import("../dist/scan.js");
    if (isRendered(command, rest)) await renderNative(argv);
    else await runNative(argv);
  }
}
