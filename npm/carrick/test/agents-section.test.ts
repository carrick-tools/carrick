// The "Carrick check" section init keeps in AGENTS.md (carrick#2828).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  AGENTS_FILE,
  hasAgentsSection,
  removeAgentsSection,
  renderCheckSection,
  withCheckSection,
  writeAgentsSection,
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
    assert.ok(body.includes("5. After you edit a file, run `carrick check <file>` on that one file"));
    assert.ok(!body.includes("When your change touches a route"));
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
