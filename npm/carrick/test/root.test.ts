import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import fs from "node:fs";
import os from "node:os";
import { ancestors, hasMarker, resolveRoot, rootNote } from "../src/root.ts";

/** A fake filesystem where only these directories hold an indexed `.carrick/`. */
function marker(...dirs: string[]) {
  const set = new Set(
    dirs.flatMap((dir) => [path.join(path.resolve(dir), ".carrick"), path.join(path.resolve(dir), ".carrick", "index.json")]),
  );
  return (target: string) => set.has(target);
}

test("the client's workspace folder wins when it holds the marker", () => {
  const choice = resolveRoot({
    clientRoot: "/ws",
    projectDir: "/ws",
    filePath: "/ws/user-service/src/a.ts",
    exists: marker("/ws"),
  });
  assert.equal(choice.root, path.resolve("/ws"));
  assert.equal(choice.source, "client");
  assert.equal(choice.differs, false);
  assert.equal(rootNote(choice), null);
});

test("a client rooted inside one service falls back to the workspace above it", () => {
  const choice = resolveRoot({
    clientRoot: "/ws/user-service",
    projectDir: null,
    filePath: "/ws/user-service/src/routes/users.ts",
    exists: marker("/ws"),
  });
  assert.equal(choice.root, path.resolve("/ws"));
  assert.equal(choice.source, "ancestor");
  assert.equal(choice.differs, true);
  assert.match(rootNote(choice) ?? "", /has no \.carrick\/; using .*ws \(ancestor\)/);
});

test("CLAUDE_PROJECT_DIR is tried before walking up from the file", () => {
  const choice = resolveRoot({
    clientRoot: "/ws/user-service",
    projectDir: "/elsewhere",
    filePath: "/ws/user-service/src/a.ts",
    exists: marker("/elsewhere", "/ws"),
  });
  assert.equal(choice.root, path.resolve("/elsewhere"));
  assert.equal(choice.source, "project_dir");
  assert.equal(choice.differs, true);
});

test("no marker anywhere keeps the client's folder and says so", () => {
  const choice = resolveRoot({
    clientRoot: "/ws/user-service",
    projectDir: null,
    filePath: "/ws/user-service/src/a.ts",
    exists: marker(),
  });
  assert.equal(choice.root, path.resolve("/ws/user-service"));
  assert.equal(choice.source, "fallback");
  assert.equal(choice.markerFound, false);
  assert.match(rootNote(choice) ?? "", /no \.carrick\/ above the file/);
});

test("ancestors run from the directory to the filesystem root, nearest first", () => {
  const list = ancestors("/a/b/c");
  assert.equal(list[0], path.resolve("/a/b/c"));
  assert.equal(list.at(-1), path.parse(path.resolve("/a/b/c")).root);
});

/** A real tree: an indexed workspace with a nested checkout whose `.carrick/` holds only a Deno cache. */
function strayTree() {
  const base = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-root-")));
  const root = path.join(base, "ws");
  const nested = path.join(root, ".claude", "worktrees", "feat");
  fs.mkdirSync(path.join(root, ".carrick"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "index.json"), "{}");
  fs.mkdirSync(path.join(nested, "apps", "api", ".carrick", "deno"), { recursive: true });
  fs.mkdirSync(path.join(nested, ".carrick", "deno"), { recursive: true });
  return { base, root, nested };
}

test("a .carrick/ that holds an index is a workspace root", () => {
  const { base, root } = strayTree();
  try {
    assert.equal(hasMarker(root), true);
    const choice = resolveRoot({ clientRoot: root, filePath: path.join(root, "a.ts") });
    assert.equal(choice.root, root);
    assert.equal(choice.markerFound, true);
  } finally {
    fs.rmSync(base, { recursive: true, force: true });
  }
});

test("a .carrick/ holding only a Deno cache is walked past, up to the real root", () => {
  const { base, root, nested } = strayTree();
  try {
    assert.equal(hasMarker(nested), false);
    for (const clientRoot of [nested, path.join(nested, "apps", "api"), null]) {
      const choice = resolveRoot({
        clientRoot,
        projectDir: nested,
        filePath: path.join(nested, "apps", "api", "src", "a.ts"),
      });
      assert.equal(choice.root, root, `client ${clientRoot}`);
      assert.equal(choice.markerFound, true);
      assert.equal(choice.source, "ancestor");
    }
  } finally {
    fs.rmSync(base, { recursive: true, force: true });
  }
});
