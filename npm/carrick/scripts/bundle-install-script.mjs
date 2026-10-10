#!/usr/bin/env node
// Copy the dependency-install script into this package, at the version of the
// binary that ships beside it.
//
// `scripts/install-scanned-deps.sh` lives at the repository root because the
// Action runs it from a checkout. A machine that installs this package from
// npm (the hosted runner, carrick#2211) needs the same script at the same
// version, and the package root is this directory, so `prepack` copies it in
// the way bundle-sidecar.mjs copies the sidecar. The copy is gitignored and
// the root file stays the source. `copyFileSync` keeps the executable bit.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.join(here, "..");
const source = path.join(packageRoot, "..", "..", "scripts", "install-scanned-deps.sh");
const target = path.join(packageRoot, "scripts", "install-scanned-deps.sh");

fs.copyFileSync(source, target);
fs.chmodSync(target, 0o755);
process.stdout.write(`bundled the install script into ${path.relative(packageRoot, target)}\n`);
