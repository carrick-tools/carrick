import assert from "node:assert/strict";
import { cpSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { Ts7Lsp } from "../src/held/engine.ts";
import { testDir } from "./helpers.ts";

/** The native compiler the `typescript-native` alias installs for this host. */
function nativeTsc(): string {
  const require = createRequire(path.join(testDir, "..", "package.json"));
  const pkg = `@typescript/typescript-${process.platform}-${process.arch}/package.json`;
  return path.join(path.dirname(require.resolve(pkg)), "lib", "tsc");
}

const shadowFixture = path.join(testDir, "fixtures", "held-shadow");

test("an error appears after set and goes after a revert", async () => {
  const cwd = mkdtempSync(path.join(os.tmpdir(), "held-engine-"));
  const engine = new Ts7Lsp(nativeTsc(), cwd);
  try {
    cpSync(shadowFixture, cwd, { recursive: true });
    const lib = path.join(cwd, "lib.ts");
    const use = path.join(cwd, "use.ts");
    const original = readFileSync(lib, "utf8");
    engine.set(lib, original);
    engine.set(use, readFileSync(use, "utf8"));

    const clean = await engine.pull([use]);
    assert.deepEqual(clean.get(use), []);

    engine.set(lib, 'export const answer: string = "one";\n');
    const broken = await engine.pull([use]);
    const items = broken.get(use) ?? [];
    assert.equal(items.length >= 1, true, "the edit to lib.ts must break use.ts");
    assert.equal(items[0]!.code, 2362);

    engine.set(lib, original);
    assert.deepEqual((await engine.pull([use])).get(use), []);

    const rss = engine.rss();
    assert.equal(typeof rss === "number" && rss > 0, true);
  } finally {
    await engine.close();
    assert.equal(engine.rss(), null);
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("a pull answered with anything but a full report throws", async () => {
  const fake = path.join(testDir, "fake-lsp.mjs");
  const cwd = mkdtempSync(path.join(os.tmpdir(), "held-engine-"));
  const engine = new Ts7Lsp(process.execPath, cwd, [fake]);
  try {
    await assert.rejects(engine.pull([path.join(cwd, "a.ts")]), /not a full report/);
  } finally {
    await engine.close();
    rmSync(cwd, { recursive: true, force: true });
  }
});
