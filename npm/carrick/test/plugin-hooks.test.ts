// The Claude Code plugin's hook manifest and `carrick init` register one list
// (carrick#4084). `carrickHooks()` is the source; `plugin/hooks/hooks.json`
// must hold exactly its entries, so a plugin-only workspace gets every hook.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { carrickHooks, flattenHooks, type HookGroup } from "../src/init/settings.ts";

const manifest = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "..",
  "plugin",
  "hooks",
  "hooks.json",
);

test("plugin hooks.json registers exactly what carrick init installs", () => {
  const plugin = JSON.parse(readFileSync(manifest, "utf8")) as {
    hooks: Record<string, HookGroup[]>;
  };
  assert.deepEqual(flattenHooks(plugin.hooks), flattenHooks(carrickHooks("carrick")));
});
