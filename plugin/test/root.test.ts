import test from "node:test";
import assert from "node:assert/strict";
import path from "node:path";
import { ancestors, resolveRoot, rootNote } from "../src/root.ts";

/** A fake filesystem where only these directories hold `.carrick/`. */
function marker(...dirs: string[]) {
  const set = new Set(dirs.map((dir) => path.join(path.resolve(dir), ".carrick")));
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
