import assert from "node:assert/strict";
import { cpSync, existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { StalePull, Ts7Lsp } from "../src/held/engine.ts";
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

/** A `Ts7Lsp` over the stand-in server in `mode`, with the server's log of sends and pulls. */
function fakeEngine(mode: string): { engine: Ts7Lsp; cwd: string; log: () => string[] } {
  const cwd = mkdtempSync(path.join(os.tmpdir(), "held-engine-"));
  const logFile = path.join(cwd, "server.log");
  const engine = new Ts7Lsp(process.execPath, cwd, [path.join(testDir, "fake-lsp.mjs"), mode, logFile]);
  return { engine, cwd, log: () => (existsSync(logFile) ? readFileSync(logFile, "utf8").trim().split("\n") : []) };
}

test("a pull answered with the previous text's report sends again and returns the report of the text just set", async () => {
  const { engine, cwd, log } = fakeEngine("stale-once");
  try {
    const file = path.join(cwd, "a.ts");
    engine.set(file, "const x = 1;\n");
    assert.deepEqual((await engine.pull([file])).get(file), []);
    engine.set(file, "const x = 1;\nERR\n");
    const items = (await engine.pull([file])).get(file) ?? [];
    assert.deepEqual(items.map((d) => [d.message, d.range.start.line]), [["fake error", 1]]);
    // One request per pull when the answer is fresh, a second only after the stale one.
    assert.equal(log().filter((l) => l === "pull").length, 3);
  } finally {
    await engine.close();
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("a server that only ever answers an older text makes the pull throw after 3 sends", async () => {
  const { engine, cwd, log } = fakeEngine("always-stale");
  try {
    const file = path.join(cwd, "a.ts");
    engine.set(file, "const x = 1;\n");
    assert.deepEqual((await engine.pull([file])).get(file), []);
    engine.set(file, "ERR\n");
    const before = log().length;
    await assert.rejects(engine.pull([file]), StalePull);
    assert.equal(log().slice(before).filter((l) => l === "pull").length, 3);
  } finally {
    await engine.close();
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("a pull of a file set in an earlier ask sends it again under a new version first", async () => {
  const { engine, cwd, log } = fakeEngine("fresh");
  try {
    const file = path.join(cwd, "a.ts");
    engine.set(file, "const x = 1;\n");
    await engine.pull([file]);
    const before = log().length;
    assert.deepEqual((await engine.pull([file])).get(file), []);
    const since = log().slice(before);
    assert.deepEqual(since, ["change 3", "pull"]);
  } finally {
    await engine.close();
    rmSync(cwd, { recursive: true, force: true });
  }
});

test("declaration and JavaScript files are sent and pulled unmarked, as they stand", async () => {
  const { engine, cwd, log } = fakeEngine("fresh");
  try {
    const files = ["a.d.ts", "b.d.mts", "c.js"].map((f) => path.join(cwd, f));
    for (const f of files) engine.set(f, "ERR\n");
    const pulled = await engine.pull(files);
    for (const f of files) assert.deepEqual((pulled.get(f) ?? []).map((d) => d.message), ["fake error"]);
    assert.deepEqual(log(), ["open 1", "open 1", "open 1", "pull", "pull", "pull"]);
  } finally {
    await engine.close();
    rmSync(cwd, { recursive: true, force: true });
  }
});

for (const real of [false, true]) {
  const where = real ? "on TypeScript 7" : "on the stand-in";
  const start = (): { engine: Ts7Lsp; cwd: string } => {
    if (!real) return fakeEngine("fresh");
    const cwd = mkdtempSync(path.join(os.tmpdir(), "held-engine-"));
    cpSync(shadowFixture, cwd, { recursive: true });
    return { engine: new Ts7Lsp(nativeTsc(), cwd), cwd };
  };

  test(`a file whose marker error can never appear (@ts-nocheck) throws, never answers empty (${where})`, async () => {
    const { engine, cwd } = start();
    try {
      const file = path.join(cwd, "nocheck.ts");
      engine.set(file, "// @ts-nocheck\nconst x: string = 1;\n");
      await assert.rejects(engine.pull([file]), StalePull);
    } finally {
      await engine.close();
      rmSync(cwd, { recursive: true, force: true });
    }
  });

  test(`a text ending in an unclosed block keeps its end-of-file error at the text's end; the marker's own error never shows (${where})`, async () => {
    const { engine, cwd } = start();
    try {
      const file = path.join(cwd, "open.ts");
      const text = "export function f(): void {\n  const n = 1;\n";
      engine.set(file, text);
      const items = (await engine.pull([file])).get(file) ?? [];
      assert.ok(!items.some((d) => /__carrick_rev|"r\d/.test(d.message)), JSON.stringify(items));
      const eof = items.filter((d) => d.message === "'}' expected.");
      assert.equal(eof.length, 1, JSON.stringify(items));
      assert.deepEqual(eof[0]!.range.start, { line: 2, character: 0 });
      assert.deepEqual(eof[0]!.range.end, { line: 2, character: 0 });
    } finally {
      await engine.close();
      rmSync(cwd, { recursive: true, force: true });
    }
  });
}
