// The copy of Carrick's hooks and skills that `carrick init` puts inside each
// repo of a folder, and the `.git/info/exclude` lines that keep it out of git
// and record what `carrick remove` takes back (carrick#1512, option A).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  EXCLUDE_BEGIN,
  EXCLUDE_END,
  EXCLUDE_PATTERNS,
  LOCAL_SETTINGS,
  excludeFile,
  ownRepository,
  removeRepoCopy,
  repoCopyPaths,
  withExcludeBlock,
  withoutExcludeBlock,
  writeFolderCopy,
  writeRepoCopy,
} from "../src/init/repo-copies.ts";
import { CODEX_HOOKS_FILE } from "../src/init/codex.ts";
import { installedCarrickHooks } from "../src/init/settings.ts";
import { taskSkillPaths } from "../src/init/task-skills.ts";

const posix = { skip: process.platform === "win32" ? "git and POSIX paths" : false };

function repo(): { dir: string; cleanup: () => void } {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-repo-copy-")));
  const dir = path.join(root, "shop-app");
  fs.mkdirSync(dir);
  execFileSync("git", ["init", "-q", dir]);
  fs.writeFileSync(path.join(dir, "package.json"), "{}\n");
  return { dir, cleanup: () => fs.rmSync(root, { recursive: true, force: true }) };
}

function status(dir: string): string {
  return execFileSync("git", ["-C", dir, "status", "--porcelain", "--untracked-files=all"], { encoding: "utf8" });
}

test("a repo's copy holds the hooks and the skills, and git sees none of it", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const before = status(dir);
    const written = writeRepoCopy(dir, { slug: "shop" });
    // The personal settings file, never the one a team commits.
    assert.deepEqual(written?.sort(), repoCopyPaths().sort());
    assert.equal(fs.existsSync(path.join(dir, ".claude", "settings.json")), false);
    assert.ok(installedCarrickHooks(fs.readFileSync(path.join(dir, LOCAL_SETTINGS), "utf8")).length > 0);
    assert.ok(fs.existsSync(path.join(dir, CODEX_HOOKS_FILE)));
    for (const skill of taskSkillPaths()) {
      assert.match(fs.readFileSync(path.join(dir, skill), "utf8"), /project: "shop"/);
    }
    // Kept out of git: the tree reads as it did.
    assert.equal(status(dir), before);
    // And a second run changes nothing, the exclude file included.
    const exclude = fs.readFileSync(excludeFile(dir)!, "utf8");
    writeRepoCopy(dir, { slug: "shop" });
    assert.equal(fs.readFileSync(excludeFile(dir)!, "utf8"), exclude);
  } finally {
    cleanup();
  }
});

test("a path the repo tracks is the team's copy, and is neither written nor excluded", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const tracked = path.join(dir, CODEX_HOOKS_FILE);
    fs.mkdirSync(path.dirname(tracked), { recursive: true });
    fs.writeFileSync(tracked, '{ "hooks": {} }\n');
    execFileSync("git", ["-C", dir, "add", CODEX_HOOKS_FILE]);
    const written = writeRepoCopy(dir, { slug: "shop" }) ?? [];
    assert.ok(!written.includes(CODEX_HOOKS_FILE), written.join("\n"));
    assert.equal(fs.readFileSync(tracked, "utf8"), '{ "hooks": {} }\n');
    assert.doesNotMatch(fs.readFileSync(excludeFile(dir)!, "utf8"), /\/\.codex\/hooks\.json/);
  } finally {
    cleanup();
  }
});

test("remove takes back exactly what the lines name, and the lines, and leaves the owner's own", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const exclude = excludeFile(dir)!;
    // The owner's own exclusion, of a path the copy would also write.
    fs.appendFileSync(exclude, "/.claude/settings.local.json\n");
    const owned = fs.readFileSync(exclude, "utf8");
    // And a hook of their own in that file, which must survive.
    fs.mkdirSync(path.join(dir, ".claude"), { recursive: true });
    fs.writeFileSync(
      path.join(dir, LOCAL_SETTINGS),
      JSON.stringify({ hooks: { Stop: [{ hooks: [{ type: "command", command: "say done" }] }] } }, null, 2),
    );
    writeRepoCopy(dir, { slug: "shop" });
    // Recorded in our block even though the owner excludes it too: the block
    // is the record of what init wrote.
    assert.equal(fs.readFileSync(exclude, "utf8").split("\n").filter((line) => line === "/.claude/settings.local.json").length, 2);

    const taken = removeRepoCopy(dir);
    assert.ok(taken !== null);
    // The owner's file exactly as it was, their line included.
    assert.equal(fs.readFileSync(exclude, "utf8"), owned);
    for (const skill of taskSkillPaths()) assert.equal(fs.existsSync(path.join(dir, skill)), false, skill);
    assert.equal(fs.existsSync(path.join(dir, CODEX_HOOKS_FILE)), false);
    assert.equal(fs.existsSync(path.join(dir, ".agents")), false);
    // Their hook stays, ours goes.
    const settings = fs.readFileSync(path.join(dir, LOCAL_SETTINGS), "utf8");
    assert.match(settings, /say done/);
    assert.deepEqual(installedCarrickHooks(settings), []);
    // And a repo with no lines of ours is not one init copied into.
    assert.equal(removeRepoCopy(dir), null);
  } finally {
    cleanup();
  }
});

test("a settings file left holding nothing goes with the copy", posix, () => {
  const { dir, cleanup } = repo();
  try {
    writeRepoCopy(dir, { slug: null });
    removeRepoCopy(dir);
    assert.equal(fs.existsSync(path.join(dir, ".claude")), false);
    assert.equal(fs.existsSync(path.join(dir, ".codex")), false);
    assert.equal(status(dir), "?? package.json\n");
  } finally {
    cleanup();
  }
});

// carrick#1512 review R1: a settings file somebody else already emptied of
// our entries — an older `carrick remove` did exactly that — is still init's
// file, and holding `{"hooks": {}}` it goes with the record that names it.
test("a recorded settings file already emptied of our hooks is deleted, not left behind", posix, () => {
  const { dir, cleanup } = repo();
  try {
    writeRepoCopy(dir, { slug: null });
    fs.writeFileSync(path.join(dir, LOCAL_SETTINGS), '{\n  "hooks": {}\n}\n');
    removeRepoCopy(dir);
    assert.equal(fs.existsSync(path.join(dir, LOCAL_SETTINGS)), false);
    assert.equal(status(dir), "?? package.json\n");
  } finally {
    cleanup();
  }
});

test("the exclude block is one marked run of lines, and taking it out gives the file back byte for byte", () => {
  const original = "# git's own comment\n*.log";
  const body = withExcludeBlock(original, [LOCAL_SETTINGS, CODEX_HOOKS_FILE]);
  // First in the file, so what follows is the owner's, untouched, whatever
  // it ends in.
  assert.equal(body, `${EXCLUDE_BEGIN}\n/.claude/settings.local.json\n/.codex/hooks.json\n${EXCLUDE_PATTERNS.join("\n")}\n${EXCLUDE_END}\n${original}`);
  assert.equal(withoutExcludeBlock(body).body, original);
  for (const ending of ["", "*.log\n", "*.log", "\n"]) {
    assert.equal(withoutExcludeBlock(withExcludeBlock(ending, [LOCAL_SETTINGS])).body, ending, JSON.stringify(ending));
  }
  // Rewritten in place, never written twice.
  assert.equal(withExcludeBlock(body, [LOCAL_SETTINGS, CODEX_HOOKS_FILE]), body);
  // Nothing to exclude, no block.
  assert.equal(withExcludeBlock(body, []), original);
  // Everything between the markers is the block's, whatever it is.
  assert.deepEqual(withoutExcludeBlock(`${EXCLUDE_BEGIN}\n/.codex/hooks.json\n*.tmp\n${EXCLUDE_END}\nrest`), {
    body: "rest",
    lines: ["/.codex/hooks.json", "*.tmp"],
    found: true,
  });
  // An end line somebody deleted: the block stops at the first line not ours.
  const cut = `${EXCLUDE_BEGIN}\n/.codex/hooks.json\n*.tmp\n`;
  assert.deepEqual(withoutExcludeBlock(cut), { body: "*.tmp\n", lines: ["/.codex/hooks.json"], found: true });
});

// carrick#1512 review R2: a folder inside another repository is not a repo of
// its own, and its lines would land in that repository's exclude file,
// anchored where they name nothing.
test("a folder inside another repository gets no copy, and that repository's exclude file is untouched", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const inner = path.join(dir, "packages", "web");
    fs.mkdirSync(inner, { recursive: true });
    fs.writeFileSync(path.join(inner, "package.json"), "{}\n");
    const before = fs.readFileSync(excludeFile(dir)!, "utf8");
    assert.equal(ownRepository(inner), false);
    assert.equal(writeRepoCopy(inner, { slug: "shop" }), null);
    assert.equal(fs.readFileSync(excludeFile(dir)!, "utf8"), before);
    assert.deepEqual(fs.readdirSync(inner), ["package.json"]);
    assert.equal(removeRepoCopy(inner), null);
  } finally {
    cleanup();
  }
});

// carrick#1512 review R3: every body is decided before the first write, so a
// file that cannot be merged stops the copy before anything, record or hook,
// is written.
test("a hooks file that is not JSON stops the copy before anything is written", posix, () => {
  const { dir, cleanup } = repo();
  try {
    fs.mkdirSync(path.join(dir, ".codex"));
    fs.writeFileSync(path.join(dir, CODEX_HOOKS_FILE), "{ not json");
    const exclude = fs.readFileSync(excludeFile(dir)!, "utf8");
    assert.throws(() => writeRepoCopy(dir, { slug: "shop" }));
    assert.equal(fs.readFileSync(excludeFile(dir)!, "utf8"), exclude);
    assert.equal(fs.existsSync(path.join(dir, LOCAL_SETTINGS)), false);
    for (const skill of taskSkillPaths()) assert.equal(fs.existsSync(path.join(dir, skill)), false, skill);
    assert.equal(fs.readFileSync(path.join(dir, CODEX_HOOKS_FILE), "utf8"), "{ not json");
  } finally {
    cleanup();
  }
});

test("an exclude file init created goes with the copy", posix, () => {
  const { dir, cleanup } = repo();
  try {
    fs.rmSync(excludeFile(dir)!);
    writeRepoCopy(dir, { slug: "shop" });
    assert.ok(fs.existsSync(excludeFile(dir)!));
    removeRepoCopy(dir);
    assert.equal(fs.existsSync(excludeFile(dir)!), false);
    assert.equal(status(dir), "?? package.json\n");
  } finally {
    cleanup();
  }
});

// carrick#4031: a running check writes temporary files beside the code it reads.
const HERMETIC_GIT: NodeJS.ProcessEnv = {
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  XDG_CONFIG_HOME: fs.mkdtempSync(path.join(os.tmpdir(), "carrick-no-git-config-")),
};

function hermeticStatus(dir: string): string {
  return execFileSync("git", ["-C", dir, "status", "--porcelain", "-uall"], { encoding: "utf8", env: { ...process.env, ...HERMETIC_GIT } });
}

const SURFACE_FILES = ["src/__carrick_surface__.4242.1.ts", "src/deep/__carrick_surface_probe_4242_0.ts"];

function writeSurfaceFiles(dir: string): void {
  for (const relative of SURFACE_FILES) {
    fs.mkdirSync(path.dirname(path.join(dir, relative)), { recursive: true });
    fs.writeFileSync(path.join(dir, relative), "export {};\n");
  }
  fs.writeFileSync(path.join(dir, "src", "keep.ts"), "export {};\n");
}

test("a single repo's block holds the surface pattern after its paths, and git shows no temporary file", posix, () => {
  const { dir, cleanup } = repo();
  try {
    assert.ok(writeFolderCopy(dir, { slug: "shop" }) !== null);
    const lines = fs.readFileSync(excludeFile(dir)!, "utf8").split("\n");
    const end = lines.indexOf(EXCLUDE_END);
    assert.deepEqual(lines.slice(end - EXCLUDE_PATTERNS.length, end), EXCLUDE_PATTERNS);
    assert.equal(lines[end - EXCLUDE_PATTERNS.length - 1]?.startsWith("/"), true);
    writeSurfaceFiles(dir);
    const shown = hermeticStatus(dir);
    assert.doesNotMatch(shown, /__carrick_surface/);
    assert.match(shown, /src\/keep\.ts/);
  } finally {
    cleanup();
  }
});

test("a repo copy in a multi-repo folder gets the same line", posix, () => {
  const { dir, cleanup } = repo();
  try {
    assert.ok(writeRepoCopy(dir, { slug: "shop" }) !== null);
    assert.ok(fs.readFileSync(excludeFile(dir)!, "utf8").split("\n").includes("__carrick_surface_*"));
    writeSurfaceFiles(dir);
    assert.doesNotMatch(hermeticStatus(dir), /__carrick_surface/);
  } finally {
    cleanup();
  }
});

test("remove takes the pattern with the block, deletes no surface file, and leaves the owner's lines", posix, () => {
  const { dir, cleanup } = repo();
  try {
    const exclude = excludeFile(dir)!;
    fs.appendFileSync(exclude, "*.log\n");
    const owned = fs.readFileSync(exclude, "utf8");
    writeRepoCopy(dir, { slug: "shop" });
    writeSurfaceFiles(dir);
    const taken = removeRepoCopy(dir);
    assert.ok(taken !== null);
    assert.equal(taken.some((relative) => relative.includes("__carrick_surface")), false);
    assert.equal(fs.readFileSync(exclude, "utf8"), owned);
    for (const relative of SURFACE_FILES) assert.ok(fs.existsSync(path.join(dir, relative)), relative);
  } finally {
    cleanup();
  }
});

test("a block missing its closing marker still ends after the pattern line", () => {
  const cut = `${EXCLUDE_BEGIN}\n/.codex/hooks.json\n${EXCLUDE_PATTERNS.join("\n")}\n*.tmp\n`;
  assert.deepEqual(withoutExcludeBlock(cut), {
    body: "*.tmp\n",
    lines: ["/.codex/hooks.json", ...EXCLUDE_PATTERNS],
    found: true,
  });
});

test("a block with no paths is no block, and carries no pattern", () => {
  assert.equal(withExcludeBlock("*.log\n", []), "*.log\n");
  assert.equal(withExcludeBlock(null, []), "");
  assert.equal(withExcludeBlock(withExcludeBlock("*.log\n", [LOCAL_SETTINGS]), []), "*.log\n");
});

test("the pattern covers every name the compiler engine writes for a running check", posix, () => {
  const sidecar = path.join(import.meta.dirname, "..", "..", "..", "src", "sidecar", "src");
  const capture = fs.readFileSync(path.join(sidecar, "capture", "index.ts"), "utf8");
  const claims = fs.readFileSync(path.join(sidecar, "library-claims.ts"), "utf8");
  const entry = /const SURFACE_ENTRY_BASENAME = '([^']+)'/.exec(capture)?.[1];
  const probe = /`(__carrick_surface_probe_)\$\{/.exec(claims)?.[1];
  assert.ok(entry && probe, "both writers found");
  const { dir, cleanup } = repo();
  try {
    fs.writeFileSync(path.join(dir, ".gitignore"), withExcludeBlock(null, [LOCAL_SETTINGS]));
    for (const name of [`${entry}.1.2.ts`, `${entry}.1.2.d.ts`, `${probe}1_0.ts`]) {
      const target = path.join("src", "a", name);
      const out = execFileSync("git", ["-C", dir, "check-ignore", target], { encoding: "utf8", env: { ...process.env, ...HERMETIC_GIT } });
      assert.equal(out.trim(), target.split(path.sep).join("/"));
    }
  } finally {
    cleanup();
  }
});
