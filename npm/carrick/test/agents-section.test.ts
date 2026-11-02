// The "Carrick check" section init keeps in AGENTS.md (carrick#2828).

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import {
  AGENTS_FILE,
  CHECK_SECTION,
  CLAUDE_FILE,
  hasAgentsSection,
  ownCarrickHeading,
  removeAgentsSection,
  RULES_END,
  RULES_START,
  rulesSection,
  withoutRulesSection,
  writeAgentsSection,
  writeClaudeSection,
} from "../src/init/agents-section.ts";

const SCOPE = { slug: "acme-index" };

function folder(): { dir: string; file: string } {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "carrick-agents-section-"));
  return { dir, file: path.join(dir, AGENTS_FILE) };
}

test("a missing AGENTS.md is created holding the check section", () => {
  const { dir, file } = folder();
  try {
    assert.equal(writeAgentsSection(dir, SCOPE), "written");
    const body = fs.readFileSync(file, "utf8");
    assert.equal(body, `${CHECK_SECTION}\n${rulesSection(SCOPE)}`);
    assert.ok(body.includes("## Carrick check\n\nBefore you say a change is done, run `carrick check --changed` alongside the tests. The per-edit check covers only the file each edit touched;"));
    assert.ok(body.includes("use the carrick-reuse skill on them."));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("an existing file keeps every line of its own, and a re-run changes nothing", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\n## Carrick\n\nAsk Carrick first.\n\n## Style\n\nTabs.\n";
    fs.writeFileSync(file, theirs);
    assert.equal(writeAgentsSection(dir, SCOPE), "written");
    const first = fs.readFileSync(file, "utf8");
    assert.ok(first.startsWith(theirs));
    // Their unmarked "## Carrick" is theirs: the check block goes in, our rules
    // do not.
    assert.ok(first.endsWith(CHECK_SECTION));
    assert.equal(first.includes(RULES_START), false);
    assert.equal(writeAgentsSection(dir, SCOPE), "unchanged");
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
    assert.equal(writeAgentsSection(dir, SCOPE), "written");
    assert.equal(
      fs.readFileSync(file, "utf8"),
      `# Top\n\n${CHECK_SECTION}\n${rulesSection(SCOPE)}\n## After\n\nmine\n`,
    );
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("remove takes out our section only, and the file where it was all there was", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\nKeep me.\n";
    fs.writeFileSync(file, theirs);
    writeAgentsSection(dir, SCOPE);
    assert.ok(hasAgentsSection(dir));
    assert.equal(removeAgentsSection(dir), "section removed");
    assert.equal(fs.readFileSync(file, "utf8"), theirs);
    assert.equal(hasAgentsSection(dir), false);
    assert.equal(removeAgentsSection(dir), null);

    fs.rmSync(file);
    writeAgentsSection(dir, SCOPE);
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

test("the rules block sits after the check block, is replaced in place and never doubles", () => {
  const { dir, file } = folder();
  try {
    assert.equal(writeAgentsSection(dir, SCOPE), "written");
    const first = fs.readFileSync(file, "utf8");
    assert.equal(first.indexOf(RULES_START) > first.indexOf(CHECK_SECTION) + CHECK_SECTION.length, true);
    assert.equal(first.split(RULES_START).length, 2);
    assert.equal(first.split(RULES_END).length, 2);
    // A scope change rewrites the block where it is.
    assert.equal(writeAgentsSection(dir, { slug: "other-index" }), "written");
    const second = fs.readFileSync(file, "utf8");
    assert.equal(second.split(RULES_START).length, 2);
    assert.match(second, /project: "other-index"/);
    assert.doesNotMatch(second, /acme-index/);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("the rules heading does not read as a hand-written Carrick section", () => {
  assert.match(rulesSection(SCOPE), /^<!-- carrick:rules start -->\n## Carrick: use it while you work\n/);
  assert.equal(ownCarrickHeading(rulesSection(SCOPE)), false);
  assert.equal(ownCarrickHeading(CHECK_SECTION), false);
  assert.equal(ownCarrickHeading("# T\n\n## Carrick\n\nx\n"), true);
});

test("a null slug renders the repo form, and a slug renders the project form", () => {
  const named = rulesSection(SCOPE);
  assert.match(named, /Pass `project: "acme-index"`/);
  assert.match(named, /every service in the project \*\*acme-index\*\*/);
  assert.doesNotMatch(named, /workspace \*\*/);
  assert.doesNotMatch(named, /Connect the agent/);
  const bare = rulesSection({ slug: null });
  assert.match(bare, /Pass `repo: "<owner\/repo>"`/);
  assert.doesNotMatch(bare, /project: "/);
  assert.ok(bare.includes("Carrick checked: <query> -> <result count>"), "the receipt keeps its ASCII arrow");
});

test("the rules name get_operation and get_callers before an operation changes", () => {
  assert.ok(
    rulesSection(SCOPE).includes(
      "Before you change an operation (an HTTP route, a GraphQL field, a socket event or a pub/sub topic), a request or response shape, or an exported function, call `get_operation` or `get_callers` and update or clear every consumer it lists, in every repo.",
    ),
  );
});

test("additivity: the body minus the rules block is today's check block byte for byte", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\nKeep me.\n";
    fs.writeFileSync(file, theirs);
    writeAgentsSection(dir, SCOPE);
    const body = fs.readFileSync(file, "utf8");
    assert.equal(withoutRulesSection(body).body, `${theirs}\n${CHECK_SECTION}`);
    assert.equal(withoutRulesSection(body).changed, true);
    assert.equal(withoutRulesSection(`${theirs}\n${CHECK_SECTION}`).changed, false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("remove takes both blocks and deletes a file they were all of", () => {
  const { dir, file } = folder();
  try {
    const theirs = "# Repo\n\nKeep me.\n";
    fs.writeFileSync(file, theirs);
    writeAgentsSection(dir, SCOPE);
    assert.equal(removeAgentsSection(dir), "section removed");
    assert.equal(fs.readFileSync(file, "utf8"), theirs);
    fs.rmSync(file);
    writeAgentsSection(dir, SCOPE);
    assert.equal(removeAgentsSection(dir), "file removed");
    assert.equal(fs.existsSync(file), false);
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a file holding only the rules block is found and removed too", () => {
  const { dir, file } = folder();
  try {
    fs.writeFileSync(file, `# Repo\n\n${rulesSection(SCOPE)}`);
    assert.equal(hasAgentsSection(dir), true);
    assert.equal(removeAgentsSection(dir), "section removed");
    assert.equal(fs.readFileSync(file, "utf8"), "# Repo\n");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("CLAUDE.md: a file that does not import AGENTS.md gets both blocks, one that does gets none, an absent one stays absent", () => {
  const { dir } = folder();
  const claude = path.join(dir, CLAUDE_FILE);
  try {
    assert.equal(writeClaudeSection(dir, SCOPE), null);
    assert.equal(fs.existsSync(claude), false);

    fs.writeFileSync(claude, "# Mine\n\n@AGENTS.md\n");
    assert.equal(writeClaudeSection(dir, SCOPE), null);
    assert.equal(fs.readFileSync(claude, "utf8"), "# Mine\n\n@AGENTS.md\n");

    fs.writeFileSync(claude, "# Mine\n\nrules\n");
    assert.equal(writeClaudeSection(dir, SCOPE), "written");
    assert.equal(fs.readFileSync(claude, "utf8"), `# Mine\n\nrules\n\n${CHECK_SECTION}\n${rulesSection(SCOPE)}`);
    assert.equal(writeClaudeSection(dir, SCOPE), "unchanged");

    assert.equal(hasAgentsSection(dir, CLAUDE_FILE), true);
    assert.equal(removeAgentsSection(dir, CLAUDE_FILE), "section removed");
    assert.equal(fs.readFileSync(claude, "utf8"), "# Mine\n\nrules\n");
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});

test("a file with an unmarked Carrick heading gets the check block and no rules block", () => {
  const { dir } = folder();
  const claude = path.join(dir, CLAUDE_FILE);
  try {
    fs.writeFileSync(claude, "# Mine\n\n## Carrick\n\nask first\n");
    assert.equal(writeClaudeSection(dir, SCOPE), "written");
    const body = fs.readFileSync(claude, "utf8");
    assert.equal(body.includes(RULES_START), false);
    assert.ok(body.endsWith(CHECK_SECTION));
  } finally {
    fs.rmSync(dir, { recursive: true, force: true });
  }
});
