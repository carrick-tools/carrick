// The contract kind (`k: "contract"`): the switch, ingest into the lazily read
// `contracts` table, whole-row checks, a file built without the table, the
// refresh's `known_contract` rule. Wire: carrick-cloud
// `docs/internal/reference/edit-facts-wire.md`, section `k: "contract"`.

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fixturePath } from "./helpers.ts";
import { buildFacts, factsPath, loadSqlite, openFacts, LOCAL_LAYOUT } from "../src/facts/store.ts";
import { contractLinesEnabled } from "../src/facts/enabled.ts";
import { fetchFactsNow, refreshFacts } from "../src/facts/fetch.ts";

const WIRE = fixturePath("edit-facts.s1.ndjson");
const SLUG = "demo-shop";

/** A workspace of the fixture's project with a fact file built from `ndjson`, or none. */
async function workspace(t: { after: (fn: () => void) => void }, ndjson: string | null, options: { contract?: boolean } = {}) {
  const root = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "carrick-contract-")));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  fs.mkdirSync(path.join(root, ".carrick", "hosted"), { recursive: true });
  fs.writeFileSync(path.join(root, ".carrick", "hosted", "snapshot.json"), JSON.stringify({ resolution: { repos: [{ project_slug: SLUG }] } }));
  if (ndjson !== null) await buildFacts(ndjson, factsPath(root), [], new Date().toISOString(), options);
  return root;
}

const wireText = () => fs.readFileSync(WIRE, "utf8");
const wireContracts = () =>
  wireText().split("\n").filter(Boolean).slice(1).map((line) => JSON.parse(line) as Record<string, any>).filter((row) => row["k"] === "contract");

/** Drop the `contracts` table from a built file: what an older package built. */
async function dropContracts(root: string): Promise<void> {
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root));
  db.exec("DROP TABLE contracts");
  db.close();
}

const answer = (body: Record<string, unknown>) => new Response(JSON.stringify(body));

test("the contract switch is on by default: off only for exactly \"0\", or with fact lines off", () => {
  assert.equal(contractLinesEnabled({}), true, "unset");
  assert.equal(contractLinesEnabled({ CARRICK_CONTRACT_LINES: "" }), true, "empty");
  assert.equal(contractLinesEnabled({ CARRICK_CONTRACT_LINES: "0" }), false, "0");
  assert.equal(contractLinesEnabled({ CARRICK_CONTRACT_LINES: "true" }), true, "true");
  assert.equal(contractLinesEnabled({ CARRICK_CONTRACT_LINES: "1" }), true, "1");
  assert.equal(contractLinesEnabled({ CARRICK_CONTRACT_LINES: "1", CARRICK_FACT_LINES: "0" }), false, "1 with facts off");
  assert.equal(contractLinesEnabled({ CARRICK_FACT_LINES: "0" }), false, "unset with facts off");
});

test("the local layout is 5: the matched kind added a table (contract)", () => {
  assert.equal(LOCAL_LAYOUT, 5);
});

test("every contract row of the pinned fixture is ingested and answered by file", async (t) => {
  const root = await workspace(t, wireText());
  const wire = wireContracts();
  assert.ok(wire.length > 0, "the fixture holds contract rows");
  const sqlite = (await loadSqlite())!;
  const db = new sqlite.DatabaseSync(factsPath(root), { readOnly: true });
  assert.equal(Number(db.prepare("SELECT COUNT(*) AS n FROM contracts").get()!["n"]), wire.length);
  db.close();
  const store = (await openFacts(root))!;
  const first = wire[0]!;
  const read = store.contracts(first["repo"], first["file"]);
  assert.deepEqual(read, wire.filter((row) => row["repo"] === first["repo"] && row["file"] === first["file"]));
  assert.equal(read[0]!.groups[0]!.lead, first["groups"][0]["lead"]);
  assert.deepEqual(store.contracts("no-such-repo", "x.ts"), []);
  store.close();
});

test("a file built without the contracts table opens as before and answers no contract rows", async (t) => {
  const root = await workspace(t, wireText());
  await dropContracts(root);
  const store = await openFacts(root);
  assert.ok(store, "opens");
  const first = wireContracts()[0]!;
  assert.deepEqual(store.contracts(first["repo"], first["file"]), []);
  // The op rows read as always.
  assert.ok(store.repos.length >= 0);
  assert.ok(store.byName("catalog-api", "getCatalogItem").length > 0);
  store.close();
});

test("a contract row is stored only when whole", async (t) => {
  const rows = wireContracts();
  const good = rows[0]!;
  const bad = [
    { ...good, groups: [] },
    { ...good, direction: "sideways" },
    { ...good, locator: undefined },
    { ...good, groups: [{ ...good["groups"][0], shown: 9 }] },
    { ...good, groups: [{ ...good["groups"][0], lead: null }] },
    { ...good, groups: [{ ...good["groups"][0], places: [{ repo: "x" }] }] },
    { ...good, untyped: "none" },
    { ...good, file: "" },
  ];
  const [header, ...rest] = wireText().split("\n").filter(Boolean);
  const keep = rest.filter((line) => !line.includes('"k":"contract"'));
  const text = `${[header, ...keep, JSON.stringify(good), ...bad.map((row) => JSON.stringify(row))].join("\n")}\n`;
  const root = await workspace(t, text);
  const store = (await openFacts(root))!;
  assert.equal(store.contracts(good["repo"], good["file"]).length, 1);
  store.close();
});

test("a request-direction group prints nothing and needs no wording", async (t) => {
  const good = wireContracts()[0]!;
  const request = { ...good, direction: "request", groups: [{ ...good["groups"][0], shown: 0, lead: null, more: null, tail: null }] };
  const [header] = wireText().split("\n");
  const root = await workspace(t, `${header}\n${JSON.stringify(request)}\n`);
  const store = (await openFacts(root))!;
  assert.equal(store.contracts(good["repo"], good["file"]).length, 1);
  store.close();
});

test("the refresh sends known_contract only when the stored file carries contract rows", async (t) => {
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return answer({ unchanged: true });
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  // A file built with no contract flag: the body is today's, whatever the rows.
  const plain = await workspace(t, wireText());
  assert.equal(await refreshFacts(plain, { request, env }), "unchanged");
  assert.equal("known_digest" in bodies[0]!, true);
  assert.equal("known_contract" in bodies[0]!, false);
  // A file the cloud said carries contract rows.
  const flagged = await workspace(t, wireText(), { contract: true });
  assert.equal(await refreshFacts(flagged, { request, env }), "unchanged");
  assert.equal(bodies[1]!["known_contract"], true);
  // The flag follows the file, not the switch.
  assert.equal(await refreshFacts(flagged, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1" } }), "unchanged");
  assert.equal(bodies[2]!["known_contract"], true);
});

test("an answer carrying contract: true is remembered with the file", async (t) => {
  const root = await workspace(t, null);
  const gz = wireText();
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (url: string, init?: RequestInit) => {
    if (String(url).includes("amazonaws.com")) return new Response(gz);
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    bodies.push(body);
    if (body["known_digest"]) return answer({ unchanged: true });
    return answer({ staged_url: "https://bucket.s3.amazonaws.com/f.ndjson", contract: true });
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  assert.equal(await refreshFacts(root, { request, env }), "written");
  assert.equal("known_contract" in bodies[0]!, false);
  const store = (await openFacts(root))!;
  assert.equal(store.meta["contract"], "1");
  store.close();
  assert.equal(await refreshFacts(root, { request, env }), "unchanged");
  assert.equal(bodies[1]!["known_contract"], true);
  // An answer without the flag leaves the new file unflagged.
  const plain = await workspace(t, null);
  const noFlag = (async (url: string) => (String(url).includes("amazonaws.com") ? new Response(gz) : answer({ staged_url: "https://bucket.s3.amazonaws.com/f.ndjson" }))) as typeof fetch;
  assert.equal(await refreshFacts(plain, { request: noFlag, env }), "written");
  const unflagged = (await openFacts(plain))!;
  assert.equal(unflagged.meta["contract"], undefined);
  unflagged.close();
});

test("with the switch on (the default), a file without the contracts table is not known; otherwise it is", async (t) => {
  const bodies: Array<Record<string, unknown>> = [];
  const request = (async (_url: string, init?: RequestInit) => {
    bodies.push(JSON.parse(String(init?.body)) as Record<string, unknown>);
    return answer({ success: false, error: { code: "facts_building" } });
  }) as typeof fetch;
  const env = { ...process.env, CARRICK_TOKEN: "t" };
  const root = await workspace(t, wireText());
  await dropContracts(root);
  await refreshFacts(root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "0" } });
  assert.equal(bodies[0]!["known_digest"] !== undefined, true, "switch off: known");
  await refreshFacts(root, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1" } });
  assert.equal("known_digest" in bodies[1]!, false, "switch on, no table: not known");
  const withTable = await workspace(t, wireText());
  await refreshFacts(withTable, { request, env: { ...env, CARRICK_CONTRACT_LINES: "1" } });
  assert.equal(bodies[2]!["known_digest"] !== undefined, true, "switch on, table present: known");
});

test("a waited fetch asks again while the cloud is building the file, and stops at its limit", async (t) => {
  const building = () => new Response(JSON.stringify({ success: false, error: { code: "facts_building", message: "building", retriable: true } }), { status: 409 });
  const env = { ...process.env, CARRICK_TOKEN: "t", CARRICK_NO_FACTS_REFRESH: "" };
  // Two asks find the build in flight, the third finds the file.
  const root = await workspace(t, null);
  let asks = 0;
  const pauses: number[] = [];
  const request = (async (url: string) => {
    if (String(url).includes("amazonaws.com")) return new Response(wireText());
    asks += 1;
    return asks < 3 ? building() : answer({ staged_url: "https://bucket.s3.amazonaws.com/f.ndjson" });
  }) as typeof fetch;
  const sleep = async (ms: number) => void pauses.push(ms);
  assert.equal(await fetchFactsNow(root, { request, env, sleep, pauseMs: 10, waitMs: 100 }), "written");
  assert.equal(asks, 3);
  assert.deepEqual(pauses, [10, 10]);
  assert.notEqual(await openFacts(root), null, "the file is on this machine when the call returns");
  // A build that never finishes is asked about for the limit and no longer.
  const stuck = await workspace(t, null);
  let stuckAsks = 0;
  const never = (async () => {
    stuckAsks += 1;
    return building();
  }) as typeof fetch;
  assert.equal(await fetchFactsNow(stuck, { request: never, env, sleep, pauseMs: 10, waitMs: 30 }), "retry");
  assert.equal(stuckAsks, 3);
  // An answer that is not a build in flight is not asked for again.
  let refused = 0;
  const none = (async () => {
    refused += 1;
    return new Response(JSON.stringify({ success: false, error: { code: "facts_unavailable", retriable: false } }), { status: 503 });
  }) as typeof fetch;
  assert.equal(await fetchFactsNow(stuck, { request: none, env, sleep, pauseMs: 10, waitMs: 100 }), "none");
  assert.equal(refused, 1);
  // The tests' own switch keeps it off the network.
  assert.equal(await fetchFactsNow(stuck, { request: never, env: { ...env, CARRICK_NO_FACTS_REFRESH: "1" }, sleep }), "none");
  assert.equal(stuckAsks, 3);
});
