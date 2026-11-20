// The "Carrick check" section init keeps in AGENTS.md (carrick#2828).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";

import {
  AGENTS_FILE,
  CLAUDE_FILE,
  hasAgentsSection,
  hasClaudeSection,
  removeAgentsSection,
  removeClaudeSection,
  renderCheckSection,
  sameExceptCounts,
  withCheckSection,
  writeAgentsSection,
  writeClaudeSection,
} from "../src/init/agents-section.ts";

const SCOPE = { slug: "shop-system" };
const COUNTS = { repos: 3, services: 43 };
const SECTION = renderCheckSection(SCOPE, COUNTS);

function folder(): { dir: string; file: string } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-agents-section-"));
  return { dir, file: path.join(dir, AGENTS_FILE) };
}

test("a missing AGENTS.md is created holding the check section", () => {
  const { dir, file } = folder();
  try {
    assert.equal(writeAgentsSection(dir, SCOPE, COUNTS), "written");
    const body = fs.readFileSync(file, "utf8");
    assert.equal(body, SECTION);
    assert.ok(body.includes("## Carrick: use it while you work, not only at the end\n\nThis workspace (3 repositories, 43 services) is indexed by Carrick. The\n"));
    // Rule 5 asks for the branch-wide check only where another service reads
    // the change, and leaves the per-file check to the edit hook: the same
    // contract check rate as the per-file wording (4 of 7) with no duplicate
    // per-file calls (carrick-cloud#3068, impact-2026-11-06 rule5b, D13).
    assert.ok(
      body.includes(
        "5. Run `carrick check --changed` before you say it is done only when your change\n   touches a route, a call to another service, an event or a type another\n   service reads. Each file you edit is already checked for you, so a change no\n   other service sees needs nothing more. If Carrick found a problem or changed\n   what you did, say so in one line in your final summary. Otherwise do not\n   mention it.\n<!-- carrick:check end -->",
      ),
    );
    assert.ok(!body.includes("carrick check <file>"));
    assert.ok(!body.includes("## Carrick check\n"));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("an existing file keeps every line of its own, and a re-run changes nothing", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\n## Carrick\n\nAsk Carrick first.\n\n## Style\n\nTabs.\n";
    fs.writeFileSync(file, theirs);
    assert.equal(writeAgentsSection(dir, SCOPE, COUNTS), "written");
    const first = fs.readFileSync(file, "utf8");
    assert.ok(first.startsWith(theirs));
    assert.ok(first.endsWith(SECTION));
    assert.equal(writeAgentsSection(dir, SCOPE, COUNTS), "unchanged");
    assert.equal(fs.readFileSync(file, "utf8"), first);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a re-run replaces our section in place and leaves what surrounds it", () => {
  const { dir, file } = folder();
  try {
    const stale = "<!-- carrick:check start -->\n## Carrick check\n\nold words\n<!-- carrick:check end -->\n";
    fs.writeFileSync(file, `# Top\n\n${stale}\n## After\n\nmine\n`);
    assert.equal(writeAgentsSection(dir, SCOPE, COUNTS), "written");
    assert.equal(fs.readFileSync(file, "utf8"), `# Top\n\n${SECTION}\n## After\n\nmine\n`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a re-run rewrites the old rule 5 in an existing section", () => {
  const { dir, file } = folder();
  try {
    writeAgentsSection(dir, SCOPE, COUNTS);
    const old = SECTION.replace(
      "If Carrick found a problem or changed\n   what you did, say so in one line in your final summary. Otherwise do not\n   mention it.",
      "In your final summary, say what\n   Carrick found and what you changed because of it.",
    );
    assert.notEqual(old, SECTION);
    fs.writeFileSync(file, old);
    assert.equal(writeAgentsSection(dir, SCOPE, COUNTS), "written");
    assert.equal(fs.readFileSync(file, "utf8"), SECTION);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("remove takes out our section only, and the file where it was all there was", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\nKeep me.\n";
    fs.writeFileSync(file, theirs);
    writeAgentsSection(dir, SCOPE, COUNTS);
    assert.ok(hasAgentsSection(dir));
    assert.equal(removeAgentsSection(dir), "section removed");
    assert.equal(fs.readFileSync(file, "utf8"), theirs);
    assert.equal(hasAgentsSection(dir), false);
    assert.equal(removeAgentsSection(dir), null);

    fs.rmSync(file);
    writeAgentsSection(dir, SCOPE, COUNTS);
    assert.equal(removeAgentsSection(dir), "file removed");
    assert.equal(fs.existsSync(file), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a file with a Carrick section of its own and no markers is not ours to remove", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\n## Carrick check\n\nWritten by hand.\n";
    fs.writeFileSync(file, theirs);
    assert.equal(hasAgentsSection(dir), false);
    assert.equal(removeAgentsSection(dir), null);
    assert.equal(fs.readFileSync(file, "utf8"), theirs);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("withCheckSection: empty body, old section in place, and no markers", () => {
  assert.equal(withCheckSection(null, SECTION), SECTION);
  assert.equal(withCheckSection("  \n", SECTION), SECTION);
  const old = "<!-- carrick:check start -->\n## Carrick check\n\nWhen your change touches a route, old.\n<!-- carrick:check end -->\n";
  assert.equal(withCheckSection(`# Top\n\n${old}\n## After\n`, SECTION), `# Top\n\n${SECTION}\n## After\n`);
  assert.equal(withCheckSection("# Mine\nlines", SECTION), `# Mine\nlines\n\n${SECTION}`);
});

test("the scope clause is the project slug, or the repo form without one", () => {
  assert.ok(SECTION.includes('Pass `project: "shop-system"` on every Carrick\ncall.'));
  const bare = renderCheckSection({ slug: null }, COUNTS);
  assert.ok(bare.includes('Pass `repo: "<owner/repo>"` on every Carrick\ncall.'));
  assert.ok(!bare.includes("project:"));
});

test("the counts sentence reads one repository and 1 service, many, and falls back when unknown", () => {
  assert.ok(renderCheckSection(SCOPE, { repos: 1, services: 1 }).includes("This workspace (one repository, 1 service) is indexed by Carrick. The\n"));
  assert.ok(SECTION.includes("This workspace (3 repositories, 43 services) is indexed by Carrick. The\n"));
  for (const unknown of [null, { repos: 2, services: 0 }]) {
    assert.ok(renderCheckSection(SCOPE, unknown).includes("This workspace is indexed by Carrick. The\n"));
  }
});

test("remove deletes the new span and nothing else", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\n## Carrick check\n\nMine.\n\n## After\n\nkeep\n";
    fs.writeFileSync(file, theirs);
    writeAgentsSection(dir, SCOPE, COUNTS);
    assert.equal(removeAgentsSection(dir), "section removed");
    assert.equal(fs.readFileSync(file, "utf8"), theirs);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// The same block in an existing CLAUDE.md (carrick#3515): a subfolder session
// never follows an `@AGENTS.md` import, so the block is written inline.
test("a team CLAUDE.md gets the block below its own text, and none is created where there is none", () => {
  const { dir } = folder();
  try {
    assert.equal(writeClaudeSection(dir, SCOPE, COUNTS), null);
    assert.ok(!fs.existsSync(path.join(dir, CLAUDE_FILE)));
    fs.writeFileSync(path.join(dir, CLAUDE_FILE), "# Team rules\n\nKeep handlers small.\n");
    assert.equal(writeClaudeSection(dir, SCOPE, COUNTS), "written");
    assert.equal(fs.readFileSync(path.join(dir, CLAUDE_FILE), "utf8"), `# Team rules\n\nKeep handlers small.\n\n${SECTION}`);
    assert.equal(writeClaudeSection(dir, SCOPE, COUNTS), "unchanged");
    assert.ok(hasClaudeSection(dir));
    assert.ok(!fs.existsSync(path.join(dir, AGENTS_FILE)));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a CLAUDE.md that only imports AGENTS.md gets the inline block too", () => {
  const { dir } = folder();
  try {
    fs.writeFileSync(path.join(dir, CLAUDE_FILE), "@AGENTS.md\n");
    assert.equal(writeClaudeSection(dir, SCOPE, COUNTS), "written");
    assert.equal(fs.readFileSync(path.join(dir, CLAUDE_FILE), "utf8"), `@AGENTS.md\n\n${SECTION}`);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("remove takes the block out of CLAUDE.md and keeps the file, even where it held only the block", () => {
  const { dir } = folder();
  const claude = path.join(dir, CLAUDE_FILE);
  try {
    fs.writeFileSync(claude, "# Team rules\n");
    writeClaudeSection(dir, SCOPE, COUNTS);
    assert.equal(removeClaudeSection(dir), "section removed");
    assert.equal(fs.readFileSync(claude, "utf8"), "# Team rules\n");
    assert.equal(removeClaudeSection(dir), null);
    fs.writeFileSync(claude, SECTION);
    assert.equal(removeClaudeSection(dir), "section removed");
    assert.ok(fs.existsSync(claude));
    assert.equal(fs.readFileSync(claude, "utf8"), "");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

// A section that differs only in its counts phrase is current (carrick#4998).
const OTHER_COUNTS = { repos: 1, services: 17 };

function hermeticGit(repo: string, ...args: string[]): string {
  return execFileSync("git", ["-C", repo, "-c", "user.email=t@example.com", "-c", "user.name=t", ...args], {
    encoding: "utf8",
    env: {
      ...process.env,
      GIT_CONFIG_GLOBAL: "/dev/null",
      GIT_CONFIG_NOSYSTEM: "1",
      XDG_CONFIG_HOME: fs.mkdtempSync(path.join(os.tmpdir(), "carrick-no-git-config-")),
    },
  });
}

test("sameExceptCounts: other counts, no counts and a changed rule line", () => {
  const a = renderCheckSection(SCOPE, { repos: 1, services: 13 });
  const b = renderCheckSection(SCOPE, OTHER_COUNTS);
  assert.notEqual(a, b);
  assert.ok(sameExceptCounts(a, b));
  assert.ok(sameExceptCounts(a, renderCheckSection(SCOPE, null)));
  assert.ok(sameExceptCounts(renderCheckSection(SCOPE, null), b));
  assert.ok(!sameExceptCounts(a, b.replace("1. Before you write", "1. First write")));
  assert.ok(!sameExceptCounts(a, renderCheckSection({ slug: "other" }, OTHER_COUNTS)));
});

test("init leaves a tracked AGENTS.md and CLAUDE.md section that differs only in counts byte for byte", () => {
  const { dir, file } = folder();
  try {
    const claude = path.join(dir, CLAUDE_FILE);
    const held = `# Team\n\n${renderCheckSection(SCOPE, { repos: 1, services: 13 })}`;
    fs.writeFileSync(file, held);
    fs.writeFileSync(claude, held);
    hermeticGit(dir, "init", "-q");
    hermeticGit(dir, "add", ".");
    hermeticGit(dir, "commit", "-q", "-m", "team");
    assert.equal(writeAgentsSection(dir, SCOPE, OTHER_COUNTS), "unchanged");
    assert.equal(writeClaudeSection(dir, SCOPE, OTHER_COUNTS), "unchanged");
    assert.equal(fs.readFileSync(file, "utf8"), held);
    assert.equal(fs.readFileSync(claude, "utf8"), held);
    assert.equal(hermeticGit(dir, "status", "--porcelain"), "");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a section with a changed rule line is rewritten with today's counts, and a new one carries the counts", () => {
  const { dir, file } = folder();
  try {
    const old = renderCheckSection(SCOPE, { repos: 1, services: 13 }).replace("1. Before you write", "1. First write");
    fs.writeFileSync(file, old);
    assert.equal(writeAgentsSection(dir, SCOPE, OTHER_COUNTS), "written");
    assert.equal(fs.readFileSync(file, "utf8"), renderCheckSection(SCOPE, OTHER_COUNTS));
    assert.equal(withCheckSection("# Team\n", renderCheckSection(SCOPE, OTHER_COUNTS)).includes("(one repository, 17 services)"), true);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
