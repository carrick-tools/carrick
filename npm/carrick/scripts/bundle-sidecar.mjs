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
// dist/src only: the sidecar's emit also holds its own test build, which is
// megabytes of fixtures nobody installs it for.
fs.cpSync(path.join(source, "dist", "src"), path.join(target, "dist", "src"), { recursive: true });
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

// What the built sidecar actually imports, which is the only list that
// matters. Its own package.json is not it: `typescript` sits in the sidecar's
// devDependencies and is imported at run time, and the release tarball only
// ever worked because the `npm ci` it ran installed dev dependencies too.
// Nothing installs inside sidecar/ here, so an undeclared import is a crash on
// a user's machine with no types and no explanation.
function importedPackages(dir) {
  const names = new Set();
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const target = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      for (const name of importedPackages(target)) names.add(name);
      continue;
    }
    if (!entry.name.endsWith(".js")) continue;
    const body = fs.readFileSync(target, "utf8");
    for (const match of body.matchAll(/^\s*(?:import|export)[^;\n]*?from\s*["']([^"'$]+)["']/gm)) {
      const specifier = match[1];
      if (specifier.startsWith(".") || specifier.startsWith("node:")) continue;
      const parts = specifier.split("/");
      names.add(specifier.startsWith("@") ? `${parts[0]}/${parts[1]}` : parts[0]);
    }
  }
  return names;
}

const carrying = JSON.parse(fs.readFileSync(path.join(packageRoot, "package.json"), "utf8"));
const declared = new Set(Object.keys(carrying.dependencies ?? {}));
const missing = [...importedPackages(path.join(target, "dist", "src"))].filter(
  (name) => !declared.has(name),
);
if (missing.length > 0) {
  process.stderr.write(
    `The built sidecar imports ${missing.sort().join(", ")}, which npm/carrick/package.json does not declare as a dependency. ` +
      `Add them: nothing installs inside sidecar/, so an undeclared import is missing at run time.\n`,
  );
  process.exit(1);
}

// The Claude Code plugin is three manifests naming this CLI's commands. It
// ships here so `--plugin-dir` names a directory an npm install actually has;
// the copy is gitignored and plugin/ stays the source.
const plugin = path.join(packageRoot, "plugin");
fs.rmSync(plugin, { recursive: true, force: true });
fs.mkdirSync(plugin, { recursive: true });
for (const entry of [".claude-plugin", ".lsp.json", "hooks"]) {
  fs.cpSync(path.join(packageRoot, "..", "..", "plugin", entry), path.join(plugin, entry), {
    recursive: true,
  });
}

// The licence travels with the tarball. npm only picks up a LICENSE file that
// sits in the package directory, and this one is the repo's.
fs.copyFileSync(path.join(packageRoot, "..", "..", "LICENSE.md"), path.join(packageRoot, "LICENSE.md"));

process.stdout.write(`bundled the sidecar into ${path.relative(packageRoot, target)}\n`);
