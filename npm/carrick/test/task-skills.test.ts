// The five task skills `carrick init` installs, and the stamp that decides
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

  assert.equal(outcomes.length, 10);
  assert.deepEqual(
    outcomes.map((row) => row.path).sort(),
    taskSkillPaths().sort(),
  );
  assert.ok(
    outcomes.every((row) => row.wrote && row.state === "absent"),
    "a first run writes all ten",
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
});

test("the whole-project duplicate ask belongs to the audit skill, and reuse says so", () => {
  // Measured (carrick#5129): asked where the project had built the same thing
  // twice, reuse read 15 of 74 groups in three texts running, while the audit's
  // units read 74 of 74. So the ask loads the audit, which runs its duplicate
  // sections only.
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const reuse = renderTaskSkill("carrick-reuse", scope);
    const audit = renderTaskSkill("carrick-audit", scope);
    const reuseDescription = /^description: (.*)$/m.exec(reuse)?.[1] ?? "";
    const auditDescription = /^description: (.*)$/m.exec(audit)?.[1] ?? "";
    // One skill claims the ask, so an agent has one to load.
    assert.match(auditDescription, /or where the whole project has built the same thing twice/);
    assert.doesNotMatch(reuseDescription, /where this project has built the same thing twice/);
    assert.match(reuseDescription, /For where the whole project has built the same thing twice, use carrick-audit\./);
    const reuseFlat = reuse.replace(/\s+/g, " ");
    assert.match(reuseFlat, /## The whole project/);
    assert.match(reuseFlat, /this skill is the wrong tool: one run cannot page the whole list and open every member/);
    assert.match(reuseFlat, /Load the carrick-audit skill/);
    assert.doesNotMatch(reuseFlat, /class what every page returned/);
    // The audit narrows itself for that ask, and still handles a project larger than one window.
    const auditFlat = audit.replace(/\s+/g, " ");
    assert.match(auditFlat, /the audit is sections C, D and E: count and estimate those three/);
    assert.match(auditFlat, /It is still the whole of C, D and E\. Every unit of the three runs/);
    assert.match(auditFlat, /Any other ask runs all five/);
    assert.match(auditFlat, /scan_from: <next_scan_from>/);
  }
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
  assert.match(census, /Where the answer carries no `phrasings`, run the second wording as its own search/);
  assert.match(census, /Do not change `similarity_threshold` while paging/);
});

test("drift proves in source whether a difference can break, pair by pair, and never says so from type texts", () => {
  // Measured: a whole-project run on the earlier body read 3 of 9 pairs, left
  // the one stored mismatch unsettled ("I did not read the router") and called
  // an upload route that parses form data the likeliest real break.
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const drift = renderTaskSkill("carrick-drift", scope).replace(/\s+/g, " ");
    const argument = scope.slug === null ? 'repo: "<owner/repo>"' : 'project: "acme-index"';
    // The pairs come from the map, which lists them in one small answer.
    assert.ok(drift.includes(`get_project_map(${argument})`), "the pairs no longer come from the map");
    assert.doesNotMatch(drift, /get_service_graph\(/);
    assert.match(drift, /## 4\. Can it break: prove it in source/);
    for (const word of ["**REAL**", "**TYPE-ONLY**", "**FALSE**", "**CAN'T TELL**"]) {
      assert.ok(drift.includes(word), `step 4 has no ${word}`);
    }
    assert.match(drift, /look at how the producer parses it \(a validator, a form or multipart parser\)/);
    assert.match(drift, /Never write that a row can break, or is likely to, from the type texts alone/);
    // A whole-project ask reads every pair: a sub-agent a pair, or one after another.
    assert.match(drift, /Load the carrick-drift skill\. Do its steps 2 to 4 for the one pair <consumer>/);
    assert.match(drift, /state pairs read against pairs the map lists/);
    assert.match(drift, /\| operation \| class \| producer type \| consumer type \| call site \| verdict \| can it break \|/);
  }
});

test("census looks up the callers of every endpoint it reached, in lots, with no cap", () => {
  // Measured: with the cap of 10, a question that reached about 60 endpoints
  // came back with callers confirmed for 13 of them.
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const census = renderTaskSkill("carrick-census", scope).replace(/\s+/g, " ");
    assert.match(census, /Do this for every endpoint step 2 reached; there is no cap/);
    assert.doesNotMatch(census, /do this for the 10 the question cares about most/);
    assert.match(census, /split them into lots of 15/);
    assert.match(census, /endpoints reached against endpoints looked up, and the two are equal/);
  }
});

test("census follows helpers to callers through the endpoint, impact greps on zero, drift reads per-call-site verdicts (#3443)", () => {
  const census = renderTaskSkill("carrick-census", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(census, /depth: 3/);
  assert.match(census, /Take the path from that endpoint, never from the handler's name/);
  assert.match(census, /chains that stop/);
  assert.match(census, /Skip `unread_sites`/);

  const impact = renderTaskSkill("carrick-impact", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(impact, /When `get_callers` returns 0 rows, grep the function's name/);
  assert.match(impact, /carries `call_site_line`/);
  assert.doesNotMatch(impact, /not the call-site line/);

  const drift = renderTaskSkill("carrick-drift", { slug: "acme-index" }).replace(/\s+/g, " ");
  assert.match(drift, /the top-level `verdicts` can be `\[\]` while the call sites hold them/);
  assert.match(drift, /from the `verdict\.state` on each call site/);
  assert.match(drift, /`verdicts` is empty and no call site carries a `verdict`/);
});

test("the audit skill renders for both scopes, counts first and states its estimate before any source", () => {
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const audit = renderTaskSkill("carrick-audit", scope).replace(/\s+/g, " ");
    const argument = scope.slug === null ? 'repo: "<owner/repo>"' : 'project: "acme-index"';
    assert.doesNotMatch(audit, /\{\{/);
    // The four counting calls are cheap on purpose: one row a list, totals unpaged.
    for (const call of [
      `get_project_map(${argument})`,
      `get_service_graph(${argument}, limit: 1)`,
      `find_similar(${argument}, audit: true, limit: 1)`,
      `get_findings(${argument}, limit: 1)`,
    ]) {
      assert.ok(audit.includes(call), `the counting step lacks ${call}`);
    }
    assert.match(audit, /Open no source yet/);
    assert.match(audit, /Carrick audit: estimate, before any source is read/);
    assert.match(audit, /Estimate: about <total> new tokens/);
    assert.match(audit, /Time: about <n> minutes with sub-agents/);
    // A run nobody can answer stops after the estimate unless the ask already said to go.
    assert.match(audit, /Cached re-reads add about 6 to 10 times that again, at a tenth of the price\./);
    assert.match(audit, /end your turn after the block and the leads with one closing line/);
    assert.match(audit, /`Say "go" to run the audit\.`/);
    assert.match(audit, /Open no source, start no sub-agent and make no further tool call in that turn/);
    assert.match(audit, /do not count again/);
    assert.match(audit, /don't ask, just run"\)\. Carry on with section 2 in the same turn\./);
    assert.match(audit, /An ask the user has narrowed to one service or some sections is not a go-ahead/);
    assert.match(audit, /Any other reply is the user's new ask/);
    assert.doesNotMatch(audit, /to run less|narrow it to one service|to narrow the audit/);
    assert.doesNotMatch(audit, /There is no cap/);
    assert.doesNotMatch(audit, /do not end your turn on a question/);
    assert.doesNotMatch(audit, /caps each section/);
    assert.match(audit, /index at/);
    assert.match(audit, /refused:/);
    assert.match(audit, /one command per shell call/);
    assert.match(audit, /including any you then worked around/);
    for (const word of ["GraphQL", "socket", "pub/sub", "queue", "RPC"]) {
      assert.ok(audit.includes(word), `the audit skill does not name ${word}`);
    }
  }
});

test("the audit skill's description keeps the words that load it for a plain question", () => {
  const audit = renderTaskSkill("carrick-audit", { slug: "acme-index" });
  const description = /^description: (.*)$/m.exec(audit)?.[1] ?? "";
  assert.match(description, /what is wrong in this codebase, what to clean up, a health check of the project, or where the whole project has built the same thing twice/);
  assert.match(description, /estimated tokens and minutes, and the leads the index already holds, before it reads any source, then waits for a go-ahead\.$/);
});

test("the audit skill sizes section A by no-route rows, never by services", () => {
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const audit = renderTaskSkill("carrick-audit", scope).replace(/\s+/g, " ");
    const argument = scope.slug === null ? 'repo: "<owner/repo>"' : 'project: "acme-index"';
    // Step 1 counts only: it does not page the unmatched rows.
    assert.match(audit, /Take the count only: do not page `unmatched_calls` now/);
    assert.match(audit, /- Calls that reach no route: <n> - Consumer and producer pairs/);
    // Section A is one unit that lists the rows itself, after the go-ahead.
    assert.ok(audit.includes(`get_service_graph(${argument}, limit: 200)`), "section A lacks the whole-project listing");
    assert.match(audit, /\*\*A\*\*: one unit, for the whole of A\. It lists the no-route rows itself\./);
    assert.match(audit, /With more than 30 it returns the row list at once and you split the rows into units of 30, each given its rows inline/);
    assert.match(audit, /Never start one sub-agent for each service/);
    assert.match(audit, /never work out an offset yourself/);
    assert.match(audit, /Pass no `service`: the whole-project listing holds every row once/);
    assert.match(audit, /where its `services` names several, the call sits in a shared file and counts once/);
    assert.match(audit, /Do not ask the index again for each service for rows you already hold/);
    assert.match(audit, /<rows read> of <rows given> no-route rows read/);
    assert.match(audit, /add up to the no-route count from step 1, with a call site counted once/);
    assert.match(audit, /listing the rows, once \(none with no such calls\) \| 97,500 \| \| \| each call that reaches no route \| 6,000/);
    assert.doesNotMatch(audit, /one unit for each service in `unmatched_by_service`/);
    assert.doesNotMatch(audit, /one unit for every 30 no-route rows from step 1/);
    assert.doesNotMatch(audit, /each service with unmatched calls/);
    assert.doesNotMatch(audit, /unmatched rows seen> of <unmatched_count>/);
    assert.doesNotMatch(audit, /services with unmatched calls/);
  }
});

test("the audit skill's estimate counts sub-agent start-up, and step 1 shows the leads the index already holds", () => {
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const audit = renderTaskSkill("carrick-audit", scope).replace(/\s+/g, " ");
    // Start-up: a line in the table, a line in the printed block, and the sub-agent count rule.
    assert.match(audit, /\| Start-up of each sub-agent \| each sub-agent the audit starts \| 40,000 \|/);
    assert.match(audit, /Sub-agents to start: <k>, about 40,000 new tokens each to start up: <n>/);
    assert.match(audit, /counting and report <n>, start-up <n>\)/);
    assert.match(audit, /First count the sub-agents the audit will start/);
    assert.match(audit, /about 27,500 new tokens on a bare setup and about 57,000 on a heavy one/);
    assert.match(audit, /it depends on how much the user's own setup puts into every sub-agent/);
    // Rates measured after start-up was taken out.
    assert.match(audit, /\| each pair \| 5,500 \|/);
    assert.match(audit, /\| each group \| 5,000 \|/);
    assert.match(audit, /\| each 100 operations served \| 132,500 \|/);
    assert.match(audit, /\| E\. One job, several answers \| once \| 73,500 \|/);
    // The cache line: a range, still at a tenth of the price; the old single figure is gone.
    assert.match(audit, /Cached re-reads add about 6 to 10 times that again, at a tenth of the price\./);
    assert.doesNotMatch(audit, /about 6 times that again/);
    assert.doesNotMatch(audit, /about six times the new tokens/);
    // The calibration sentence names both measurements and no project.
    assert.match(audit, /The rates come from two measurements with Claude Sonnet on 25 Nov 2026/);
    assert.doesNotMatch(audit, /monorepo of 17 packages/);
    // The leads: three to five lines from the four answers, no extra call, no narrowing, all protocols.
    assert.match(audit, /What the index shows so far, before any source has been read\. These are leads, not verdicts:/);
    assert.match(audit, /three to five lines/);
    assert.match(audit, /Build the lines only from the answers of the four calls above: make no further tool call and open no source for them/);
    assert.match(audit, /Each line gives a count and, where an answer carries one, one example with its file and line/);
    assert.match(audit, /Routes, events or procedures with no recorded caller/);
    assert.match(audit, /do not suggest narrowing the audit: the audit proper has not run/);
    assert.match(audit, /After the leads, take the first case that fits/);
    assert.match(audit, /end your turn after the block and the leads with one closing line: `Say "go" to run the audit\.`/);
    assert.match(audit, /write the block and the leads, then take one of the cases below/);
    // The leads sit between the block and the three cases.
    const block = audit.indexOf("Time: about <n> minutes with sub-agents");
    const leads = audit.indexOf("What the index shows so far");
    const cases = audit.indexOf("After the leads, take the first case that fits");
    assert.ok(block > 0 && block < leads && leads < cases, "the leads are not between the block and the cases");
  }
});

test("the audit skill sizes section A by no-route rows, never by services", () => {
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const audit = renderTaskSkill("carrick-audit", scope).replace(/\s+/g, " ");
    const argument = scope.slug === null ? 'repo: "<owner/repo>"' : 'project: "acme-index"';
    // Step 1 counts only: it does not page the unmatched rows.
    assert.match(audit, /Take the count only: do not page `unmatched_calls` now/);
    assert.match(audit, /- Calls that reach no route: <n> - Consumer and producer pairs/);
    // Section A is one unit that lists the rows itself, after the go-ahead.
    assert.ok(audit.includes(`get_service_graph(${argument}, limit: 200)`), "section A lacks the whole-project listing");
    assert.match(audit, /\*\*A\*\*: one unit, for the whole of A\. It lists the no-route rows itself\./);
    assert.match(audit, /With more than 30 it returns the row list at once and you split the rows into units of 30, each given its rows inline/);
    assert.match(audit, /Never start one sub-agent for each service/);
    assert.match(audit, /never work out an offset yourself/);
    assert.match(audit, /Pass no `service`: the whole-project listing holds every row once/);
    assert.match(audit, /where its `services` names several, the call sits in a shared file and counts once/);
    assert.match(audit, /Do not ask the index again for each service for rows you already hold/);
    assert.match(audit, /<rows read> of <rows given> no-route rows read/);
    assert.match(audit, /add up to the no-route count from step 1, with a call site counted once/);
    assert.match(audit, /each 30 no-route calls, rounded up \| 20,000/);
    assert.doesNotMatch(audit, /one unit for each service in `unmatched_by_service`/);
    assert.doesNotMatch(audit, /one unit for every 30 no-route rows from step 1/);
    assert.doesNotMatch(audit, /each service with unmatched calls/);
    assert.doesNotMatch(audit, /unmatched rows seen> of <unmatched_count>/);
    assert.doesNotMatch(audit, /services with unmatched calls/);
  }
});

test("the audit skill runs five sections, each from an index call, and none can be skipped", () => {
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const audit = renderTaskSkill("carrick-audit", scope).replace(/\s+/g, " ");
    const argument = scope.slug === null ? 'repo: "<owner/repo>"' : 'project: "acme-index"';
    for (const heading of [
      "## A. Calls that reach no route",
      "## B. Type verdicts, pair by pair",
      "## C. Functions written twice",
      "## D. Routes written twice",
      "## E. One job, several answers",
    ]) {
      assert.ok(audit.includes(heading), `missing ${heading}`);
    }
    // Each section's leads come from a scoped index call, never from reading the tree.
    for (const call of [
      `get_service_graph(${argument}, limit: 200)`,
      `check_compatibility(${argument}, consumer_service: "<consumer>", producer_service: "<producer>", limit: 50)`,
      `find_similar(${argument}, audit: true, limit: 50)`,
      `find_similar(${argument}, audit: true, include_callbacks: true, service: "<producer>", limit: 50)`,
      `search_by_intent(${argument}, query: "<job>", also_phrased_as: ["<the same job in other words>"], top_k: 30, compact: true)`,
    ]) {
      assert.ok(audit.includes(call), `missing or unscoped: ${call}`);
    }
    assert.match(audit, /Do not read through the code looking for problems/);
    assert.match(audit, /no_matching_producer/);
    // One sub-agent a unit where the harness has them, the same units in order where it has none.
    assert.match(audit, /Load the carrick-audit skill\. Run only section <letter> of it/);
    assert.match(audit, /Where you cannot start sub-agents, run the units yourself in the order above/);
    assert.match(audit, /Run again any unit that came back without one/);
    for (const letter of ["A", "B", "C", "D", "E"]) {
      assert.ok(audit.includes(`Count line: \`${letter}`), `section ${letter} has no count line`);
    }
    // A real item is proven, and one that is not proven is not in the fix list.
    assert.match(audit, /quotes the source line that makes it real/);
    assert.match(audit, /A group you have not proven to the end is never DIVERGED/);
    // A "different result" nobody is shown is unused code: every member needs a caller the index names.
    assert.ok(audit.includes(`get_callers(${argument}, function_name: "<name>", file: "<file>")`), "DIVERGED does not ask who calls each member");
    assert.match(audit, /is unused code, which nobody sees a result from: the group is then VARIANT/);
    assert.match(audit, /Name a caller, a count or what a file mounts only from a row or a line you hold/);
    assert.match(audit, /constants and helpers it reads/);
    assert.match(audit, /different call signature/);
    const notCovered = /Not covered yet: ([^]*?) Say so in the report/.exec(audit);
    assert.ok(notCovered, "the Not covered yet line is missing");
    assert.doesNotMatch(notCovered[1] ?? "", /calls that reach no route/);
  }
});

test("the audit skill reads every similar-function group once and starts no route unit without a candidate", () => {
  for (const scope of [{ slug: "acme-index" }, { slug: null }]) {
    const audit = renderTaskSkill("carrick-audit", scope).replace(/\s+/g, " ");
    // C: one listing unit walks the windows, units get their groups inline.
    assert.match(audit, /\*\*C\*\*: one listing unit, for the whole of C's list\./);
    assert.match(audit, /split it into units of 20 groups in the order it came back, each given its groups inline/);
    assert.match(audit, /A unit never works out an offset and never asks the index for the list again/);
    assert.doesNotMatch(audit, /offsets 0 to 19/);
    assert.doesNotMatch(audit, /first offset of the unit/);
    assert.match(audit, /offsets start again at 0 in every window/);
    assert.match(audit, /`total_clusters` is the group count of this window alone/);
    assert.match(audit, /call again with `scan_from: <next_scan_from>` and offset 0/);
    assert.match(audit, /The walk is done when an answer has no `has_more` and no `next_scan_from`/);
    assert.match(audit, /page that window again from offset 0 and keep the union by key, until a whole pass adds no new key/);
    assert.match(audit, /A group's key is its members' `file_path:line_number`, sorted and joined/);
    assert.match(audit, /a key seen again in a later window or page is the same group/);
    assert.match(audit, /Across section C the three numbers are equal: groups listed, groups given to units, groups classed/);
    assert.match(audit, /take those and list nothing: call no listing tool for them/);
    // D: its candidates cannot come from C's list; with none, no unit starts.
    assert.match(audit, /section C's walk cannot give D its candidates/);
    assert.match(audit, /Drop every other group unread/);
    assert.match(audit, /With no candidate, D starts no sub-agent and its count line says so/);
    assert.match(audit, /`D: no candidate, no unit`/);
    assert.match(audit, /candidates listed, given and classed are equal/);
    assert.doesNotMatch(audit, /\*\*D\*\*: one unit\./);
  }
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
  assert.equal(removeTaskSkills(dir).deleted.length, 10);
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
  assert.equal(result.deleted.length, 8);
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
// from what is on disk: ten findings in `carrick doctor` and a daily line
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

  assert.equal(removeTaskSkills(dir).deleted.length, 10);
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

test("every find_similar call in the reuse skill names its mode: `functions` or `audit: true`", () => {
  // The server refuses a find_similar call that names neither, so an audit
  // written as a bare call stops working (carrick-cloud#2851).
  const body = renderTaskSkill("carrick-reuse", { slug: "acme-index" });
  const calls = body.match(/find_similar\([^\n]*/g) ?? [];
  assert.ok(calls.length >= 2, `expected the targeted call and the group count, got ${calls.length}`);
  for (const call of calls) assert.match(call, /functions:|audit: true/, call);
});
