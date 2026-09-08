#!/usr/bin/env node
// Copy the built type sidecar into this package, so publishing it is one step.
//
// The sidecar is the reason a scanned repo's types are real types instead of
// `any`, and until now shipping it meant an `npm ci` inside the release
// tarball on every run (action.yml). Here its two dependencies are this
// package's dependencies, so npm installs them once with everything else and
// the copy under `sidecar/` needs no install of its own.
//
// It runs from `prepack`, which means `npm publish` and `npm pack` cannot
// produce a tarball without it: a package that shipped no sidecar would scan
// and index every endpoint with no request or response type, and nothing about
// the result would say so.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const packageRoot = path.join(here, "..");
const source = path.join(packageRoot, "..", "..", "src", "sidecar");
const target = path.join(packageRoot, "sidecar");
const entry = path.join("dist", "src", "index.js");

if (!fs.existsSync(path.join(source, entry))) {
  process.stderr.write(
    `The type sidecar is not built: ${path.join(source, entry)} does not exist.\n` +
      `Build it first:\n\n    cd src/sidecar && npm ci && npm run build\n\n`,
  );
  process.exit(1);
}

// The sidecar's own package.json carries its dependencies and its dev
// toolchain. Neither belongs in the copy: the dependencies are declared by the
// package that holds it, and an install must never run in here. `type: module`
// is the one thing the copy needs, because dist/ is ESM.
fs.rmSync(target, { recursive: true, force: true });
fs.mkdirSync(target, { recursive: true });
fs.cpSync(path.join(source, "dist"), path.join(target, "dist"), { recursive: true });
fs.writeFileSync(
  path.join(target, "package.json"),
  `${JSON.stringify(
    {
      name: "@carrick/type-sidecar-bundled",
      private: true,
      type: "module",
      main: "dist/src/index.js",
    },
    null,
    2,
  )}\n`,
);

const declared = JSON.parse(fs.readFileSync(path.join(source, "package.json"), "utf8"));
const carrying = JSON.parse(fs.readFileSync(path.join(packageRoot, "package.json"), "utf8"));
const missing = Object.entries(declared.dependencies ?? {}).filter(
  ([name]) => !(name in (carrying.dependencies ?? {})),
);
if (missing.length > 0) {
  process.stderr.write(
    `The sidecar depends on ${missing.map(([name]) => name).join(", ")}, which npm/carrick/package.json does not declare. ` +
      `Add them to its dependencies: nothing installs inside sidecar/, so an undeclared one is missing at run time.\n`,
  );
  process.exit(1);
}

process.stdout.write(`bundled the sidecar into ${path.relative(packageRoot, target)}\n`);
