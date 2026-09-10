import assert from "node:assert/strict";
import test from "node:test";
import { connectRepos } from "../src/init/connect.ts";
import type { ResolvedRepos } from "../src/auth/read.ts";

const initial: ResolvedRepos = { schema: "carrick.resolve-repos/0", workspace: { slug: "acme", billing_tier: "free", installed: false }, allowance_sentence: null, repos: [{ full_name: "acme/api", connected: false }], project_repos: [] };
const connected: ResolvedRepos = { ...initial, workspace: { ...initial.workspace, installed: true }, repos: [{ full_name: "acme/api", connected: true, project_id: "p1", project_slug: "system", services: [] }] };

test("connection polling reports the transition, not an invented hosted index", async () => {
  const lines: string[] = [];
  let polls = 0;
  const result = await connectRepos("token", ["acme/api"], initial, {
    interactive: true, say: (line) => lines.push(line), wait: async () => {},
    open: async (url) => { assert.equal(url, "https://app.carrick.tools/w/acme/connect"); return true; },
    poll: async () => ++polls === 1 ? initial : connected,
  });
  assert.equal(polls, 2);
  assert.deepEqual(result, connected);
  assert.equal(lines.filter((line) => line === "Connected acme/api.").length, 1);
});

test("connection cancellation retains the latest metadata and unattended init never polls", async () => {
  const controller = new AbortController();
  const result = await connectRepos("token", [], initial, {
    interactive: true, signal: controller.signal, say: () => {}, open: async () => false,
    wait: async () => { controller.abort(); }, poll: async () => { throw new Error("must not poll after cancellation"); },
  });
  assert.deepEqual(result, initial);
  assert.deepEqual(await connectRepos("token", [], initial, {
    interactive: false, say: () => {}, open: async () => { throw new Error("must not open"); },
  }), initial);
});
