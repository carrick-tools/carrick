#!/usr/bin/env node
// Copy the language server into the extension before packaging.
//
// The server lives in the npm package, shared with the Claude Code plugin, and
// `vsce package` only takes what is under this directory. The copy is
// gitignored: the source of truth is npm/carrick/src.
//
// This step disappears with carrick#710, when the extension's command becomes
// `carrick lsp --stdio` from the published npm package.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = path.join(here, "..", "..", "npm", "carrick", "src");
const target = path.join(here, "server");

fs.rmSync(target, { recursive: true, force: true });
fs.cpSync(source, target, { recursive: true });
process.stdout.write(`copied ${path.relative(here, source)} to ${path.relative(here, target)}\n`);
