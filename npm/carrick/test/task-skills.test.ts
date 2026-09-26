// The four task skills `carrick init` installs, and the stamp that decides
// which copies are ours.
//
// The two that can destroy something are tested hardest: a body somebody has
// edited, and a skill of the same name somebody wrote themselves. Neither is
// overwritten by an install and neither is deleted by `carrick remove`.

import assert from "node:assert/strict";
import test from "node:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFileSync } from "node:child_process";
import {
  ignoredSkillRoots,
  removeTaskSkills,
  renderTaskSkill,
  scopeOf,
  skillFile,
  skillState,
  SKILL_ROOTS,
  TASK_SKILLS,
  taskSkillWarnings,
  taskSkillPaths,
  writeTaskSkills,
} from "../src/init/task-skills.ts";
import { templatesDir } from "../src/templates.ts";

function workspace(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), "carrick-skills-"));
}

test("every skill lands under both harnesses, with its frontmatter and its stamp", () => {
  const dir = workspace();
  const outcomes = writeTaskSkills(dir, { slug: "acme-index" });

  assert.equal(outcomes.length, 8);
  assert.deepEqual(
    outcomes.map((row) => row.path).sort(),
    taskSkillPaths().sort(),
  );
  assert.ok(
    outcomes.every((row) => row.wrote && row.state === "absent"),
    "a first run writes all eight",
  );

  for (const root of SKILL_ROOTS) {
    for (const name of TASK_SKILLS) {
      const body = fs.readFileSync(path.join(dir, skillFile(root, name)), "utf8");
      assert.match(body, new RegExp(`^---\\nname: ${name}\\n`), `${name} has no frontmatter name`);
      assert.match(body, /\ndescription: \S/, `${name} has no description`);
      assert.equal(skillState(body), "ours", `${name} is not readable as ours`);
    }
  }

  // Both harnesses get the same bytes, which is the whole of Codex parity.
  const claude = fs.readFileSync(path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-impact")), "utf8");
  const codex = fs.readFileSync(path.join(dir, skillFile(SKILL_ROOTS[1]!, "carrick-impact")), "utf8");
  assert.equal(claude, codex);
});

test("a known project slug is written into the calls, and an unknown one names the remote", () => {
  const known = renderTaskSkill("carrick-census", { slug: "acme-index" });
  assert.match(known, /search_by_intent\(project: "acme-index", query:/);
  assert.doesNotMatch(known, /owner\/repo/);

  const unknown = renderTaskSkill("carrick-census", { slug: null });
  assert.match(unknown, /search_by_intent\(repo: "<owner\/repo>", query:/);
  assert.match(unknown, /git remote get-url origin/);

  // A placeholder left in a body is a command an agent would run verbatim.
  for (const name of TASK_SKILLS) {
    assert.doesNotMatch(renderTaskSkill(name, { slug: "acme-index" }), /\{\{[A-Z_]+\}\}/);
  }
});

test("the drift body classes every verdict situation get_contract_pair can return", () => {
  // A stored state with no class is a finding the skill drops. The first real
  // drift this body was written against was stored `unresolved` while the two
  // type texts differed on one field's optionality, and a body holding MATCH
  // and DRIFT alone had nowhere to put it.
  const body = renderTaskSkill("carrick-drift", { slug: "acme-index" });
  const classes = body.slice(body.indexOf("\n## 3."), body.indexOf("\n## 4."));
  assert.ok(classes.length > 0, "the drift body has no section 3");

  for (const head of [
    /\*\*MATCH\*\*: the stored verdict is `compatible`/,
    /\*\*DRIFT\*\*: the stored verdict is `incompatible`/,
    /\*\*UNRESOLVED\*\*: the stored verdict is `unresolved`/,
    /\*\*NOT JUDGED\*\*: `verdicts` is empty/,
  ]) {
    assert.match(classes, head, `no class in section 3 matches ${head}`);
  }
  // Read by the agent, so it is labelled as a reading and never as a verdict.
  assert.match(classes, /"type texts differ"/);

  const flat = body.replace(/\s+/g, " ");
  assert.match(
    flat,
    /Class words, and only these: MATCH, DRIFT, UNRESOLVED, NOT JUDGED, CONSUMER UNTYPED, PRODUCER UNTYPED\./,
  );
  assert.match(flat, /gh issue create --title "<consumer> and <producer> may disagree on/);
});

test("the reuse body classes every row find_similar returned, by tests that decide", () => {
  // Measured on the shipped body (carrick#1349): agents found the clusters and
  // then wrote the hard ones up in free wording, because three qualitative
  // class words describe the same pair equally well and nothing bound the
  // column to the rows the tool returned.
  const body = renderTaskSkill("carrick-reuse", { slug: "acme-index" });
  const classes = body.slice(body.indexOf("\n## Class every row"), body.indexOf("\n## Report"));
  assert.ok(classes.length > 0, "the reuse body has no classing section");

  for (const head of [
    /\*\*FALSE POSITIVE\*\*: the two contracts differ/,
    /\*\*VARIANT\*\*: one contract, and a behavioural difference you can name/,
    /\*\*DUPLICATE\*\*: one contract, and nothing left to name/,
  ]) {
    assert.match(classes, head, `no class in the classing section matches ${head}`);
  }
  // Ordered, so the first test that holds decides the row.
  assert.match(classes.replace(/\s+/g, " "), /take the first of these that holds/);
  // `find_similar` carries no field for a documented mirror, so the evidence is
  // a comment in the file the tool points at. The head of the file counts: a
  // run against our own index read two mirrored copies as one behaviour,
  // because the comment naming the mirror sits above the module rather than
  // above the function the span starts at.
  assert.match(
    classes.replace(/\s+/g, " "),
    /Where a comment on the member or at the head of its file names the file it mirrors, the clause is "documented mirror"\./,
  );

  const report = body.slice(body.indexOf("\n## Report"), body.indexOf("\n## Act"));
  const flat = report.replace(/\s+/g, " ");
  assert.match(flat, /The class column carries one of the three words and is never empty\./);
  assert.match(
    flat,
    /State `total_clusters` from the response against the number of clusters carrying rows above\./,
  );
});

test("impact and drift bind their verdict words to the rows their tools returned", () => {
  const impact = renderTaskSkill("carrick-impact", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(
    impact,
    /Every consumer call site the answers returned carries one of them and the verdict column is never empty, and the report states call sites returned against call sites given a verdict\./,
  );

  const drift = renderTaskSkill("carrick-drift", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(drift, /the report states operations returned against operations classed/);
  // A live pair satisfied UNRESOLVED and PRODUCER UNTYPED at once, so the words
  // are ordered rather than listed.
  assert.match(
    drift,
    /takes the first that applies of DRIFT, PRODUCER UNTYPED, CONSUMER UNTYPED, UNRESOLVED, NOT JUDGED, MATCH/,
  );
});

test("the bodies read what an answer carries rather than a response shape written here", () => {
  const reuse = renderTaskSkill("carrick-reuse", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(reuse, /`vector_basis` says what the cosines are over/);
  assert.match(reuse, /every key the answer carries under `not_compared` and under `excluded`/);
  // A body listing those keys drops the next one the lambda adds, which is how
  // it came to name four of the five it now returns.
  assert.doesNotMatch(reuse, /awaiting_embedding/);

  // The pair-level headline `type_verdicts` does not carry. A check that
  // compared nothing answers `unresolved` and no boolean at all, so a body
  // reading the buckets alone has nothing to tell that apart from agreement.
  const impact = renderTaskSkill("carrick-impact", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(impact, /Read `status` before the buckets/);
  assert.match(impact, /`pairs_compared` and `pairs_uncompared`/);

  // One call where the server answers both wordings, two where it does not.
  const census = renderTaskSkill("carrick-census", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(census, /also_phrased_as: \["<how it does it>"\]/);
  assert.match(census, /Where the answer carries no `phrasings`, or the field comes back refused/);
  assert.match(census, /`hidden_by_threshold` where the answer carries it/);

  // The other hidden count, and its lever is a different one: these rows are
  // held back by word weight, so a lower similarity_threshold never reaches
  // them and a receipt naming only the first count reads as if it would.
  assert.match(census, /`hidden_by_lexical_floor` where the answer carries it/);
  assert.match(census, /A lower `similarity_threshold` does not reach these rows\./);

  // A function with no intent is still ranked, on its own tokens, so the
  // receipt cannot report it as a function no search looked at.
  assert.doesNotMatch(census, /which no search looked at/);
  assert.match(census, /ranked on their name, signature and body tokens alone/);
});

test("a second run writes nothing and reports nothing to fix", () => {
  const dir = workspace();
  writeTaskSkills(dir, { slug: "acme-index" });
  const before = taskSkillPaths().map((relative) => fs.statSync(path.join(dir, relative)).mtimeMs);

  const again = writeTaskSkills(dir, { slug: "acme-index" });
  assert.ok(again.every((row) => row.state === "ours" && !row.wrote), "a re-run rewrote a file");

  const after = taskSkillPaths().map((relative) => fs.statSync(path.join(dir, relative)).mtimeMs);
  assert.deepEqual(after, before);
  assert.deepEqual(taskSkillWarnings(again), []);
});

test("a body somebody has edited is left alone and named", () => {
  const dir = workspace();
  writeTaskSkills(dir, { slug: "acme-index" });

  // Both ordinary edits: a step appended under the marker, and a line changed
  // in the body above it.
  const edited = path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-reuse"));
  const mine = `${fs.readFileSync(edited, "utf8")}\nMy own step.\n`;
  fs.writeFileSync(edited, mine);
  assert.equal(skillState(mine), "edited");

  const inBody = path.join(dir, skillFile(SKILL_ROOTS[1]!, "carrick-reuse"));
  const changed = fs.readFileSync(inBody, "utf8").replace("# What already does this", "# Our reuse pass");
  fs.writeFileSync(inBody, changed);
  assert.equal(skillState(changed), "edited");

  const outcomes = writeTaskSkills(dir, { slug: "acme-index" });
  assert.equal(fs.readFileSync(edited, "utf8"), mine, "an edited body was overwritten");
  assert.equal(fs.readFileSync(inBody, "utf8"), changed, "a changed body was overwritten");
  const row = outcomes.find((entry) => entry.path === skillFile(SKILL_ROOTS[0]!, "carrick-reuse"));
  assert.equal(row?.state, "edited");
  assert.equal(row?.wrote, false);
  assert.ok(
    taskSkillWarnings(outcomes).some((line) => line.includes("carrick-reuse") && line.includes("edited")),
    "nothing told the user which file was kept",
  );
});

test("a checkout that rewrote the line endings still reads as ours", () => {
  // `core.autocrlf` is on by default on Windows, so this is the ordinary state
  // of these files there rather than an exotic one.
  const dir = workspace();
  writeTaskSkills(dir, { slug: "acme-index" });
  const target = path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-drift"));
  const crlf = fs.readFileSync(target, "utf8").replace(/\n/g, "\r\n");
  fs.writeFileSync(target, crlf);

  assert.equal(skillState(crlf), "ours");
  assert.equal(removeTaskSkills(dir).deleted.length, 8);
});

test("a skill of the same name that Carrick never wrote is left alone and named", () => {
  const dir = workspace();
  const target = path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-drift"));
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.writeFileSync(target, "---\nname: carrick-drift\n---\n\nMine.\n");

  const outcomes = writeTaskSkills(dir, { slug: "acme-index" });
  assert.equal(fs.readFileSync(target, "utf8"), "---\nname: carrick-drift\n---\n\nMine.\n");
  assert.equal(outcomes.find((row) => row.path === skillFile(SKILL_ROOTS[0]!, "carrick-drift"))?.state, "theirs");
  assert.ok(taskSkillWarnings(outcomes).some((line) => line.includes("not written by Carrick")));
});

test("remove deletes the stamped copies and nothing else", () => {
  const dir = workspace();
  writeTaskSkills(dir, { slug: "acme-index" });

  const edited = path.join(dir, skillFile(SKILL_ROOTS[0]!, "carrick-impact"));
  fs.writeFileSync(edited, `${fs.readFileSync(edited, "utf8")}\nMine.\n`);
  const theirs = path.join(dir, skillFile(SKILL_ROOTS[1]!, "carrick-census"));
  fs.writeFileSync(theirs, "mine, never Carrick's\n");
  // A neighbour under the same root, which the removal must not touch.
  const neighbour = path.join(dir, SKILL_ROOTS[0]!, "my-skill", "SKILL.md");
  fs.mkdirSync(path.dirname(neighbour), { recursive: true });
  fs.writeFileSync(neighbour, "mine\n");

  const result = removeTaskSkills(dir);
  assert.equal(result.deleted.length, 6);
  assert.ok(fs.existsSync(edited), "an edited body was deleted");
  assert.ok(fs.existsSync(theirs), "somebody else's skill was deleted");
  assert.ok(fs.existsSync(neighbour), "an unrelated skill was deleted");
  assert.deepEqual(
    result.kept.map((row) => row.state).sort(),
    ["edited", "theirs"],
  );
  for (const relative of result.deleted) {
    assert.ok(!fs.existsSync(path.join(dir, relative)), `${relative} survived`);
    assert.ok(!fs.existsSync(path.dirname(path.join(dir, relative))), `${relative}'s directory survived`);
  }
  // A second removal finds nothing and says so rather than failing.
  assert.deepEqual(removeTaskSkills(dir), { deleted: [], kept: result.kept });
});

// `scopeOf` reads the scope back out of an installed body to render the same
// body again and tell an older version's file from a current one
// (carrick#1333). It takes the first `project: "…"` there, which is the scope
// only while the templates carry `{{SCOPE}}` and no literal of that shape. A
// template that grew a prose example would make every file render differently
// from what is on disk: eight findings in `carrick doctor` and a daily line
// that never stops, both of them wrong and neither of them loud.
test("no skill template holds a project scope of its own", () => {
  for (const name of TASK_SKILLS) {
    const source = fs.readFileSync(path.join(templatesDir(), "skills", `${name}.md`), "utf8");
    assert.equal(
      source.includes('project: "'),
      false,
      `${name}.md holds a literal project scope; scopeOf would read it instead of the rendered one`,
    );
  }
  // And the rendered body does hold exactly one, which is what makes the
  // assertion above worth making.
  const rendered = renderTaskSkill("carrick-impact", { slug: "acme-index" });
  assert.ok((rendered.match(/project: "acme-index"/g)?.length ?? 0) > 0);
  assert.equal(scopeOf(rendered).slug, "acme-index");
  assert.equal(scopeOf(renderTaskSkill("carrick-impact", { slug: null })).slug, null);
});

// carrick#1331. `.agents/` exists because init wrote skills into it, so an
// empty one left behind after `carrick remove` is litter a user has to
// recognise before deleting. `.claude/` is theirs and holds their settings, so
// the same call must leave it exactly where it is.
test("removing the last skill takes the host folder init created with it", () => {
  const dir = workspace();
  writeTaskSkills(dir, { slug: "acme-index" });
  fs.mkdirSync(path.join(dir, ".claude"), { recursive: true });
  fs.writeFileSync(path.join(dir, ".claude", "settings.json"), "{}\n");

  assert.equal(removeTaskSkills(dir).deleted.length, 8);
  assert.equal(fs.existsSync(path.join(dir, ".agents")), false, ".agents was left behind");
  assert.equal(fs.existsSync(path.join(dir, ".claude", "settings.json")), true);
  assert.equal(fs.existsSync(path.join(dir, ".claude")), true, "somebody's own folder went with it");
});

test("a host folder holding anything else of theirs stays", () => {
  const dir = workspace();
  writeTaskSkills(dir, { slug: "acme-index" });
  fs.writeFileSync(path.join(dir, ".agents", "notes.md"), "mine\n");

  removeTaskSkills(dir);
  assert.equal(fs.existsSync(path.join(dir, ".agents", "notes.md")), true);
});

test("an ignored skills directory is reported, and a tracked one is not", () => {
  const dir = fs.realpathSync(workspace());
  const git = (...args: string[]): void => {
    execFileSync("git", args, { cwd: dir, stdio: "ignore" });
  };
  git("init", "-q");
  git("config", "user.email", "test@example.com");
  git("config", "user.name", "test");
  fs.writeFileSync(path.join(dir, ".gitignore"), ".claude/\n");
  git("add", ".");
  git("commit", "-qm", "one");

  writeTaskSkills(dir, { slug: "acme-index" });
  assert.deepEqual(ignoredSkillRoots(dir), [SKILL_ROOTS[0]]);

  // Outside a repository git declines to answer, and this says nothing.
  assert.deepEqual(ignoredSkillRoots(workspace()), []);
});
