import assert from "node:assert/strict";
import test from "node:test";
import { connectRepos, reposAreInProject } from "../src/init/connect.ts";
import { resolveRepos, type ResolvedRepos } from "../src/auth/read.ts";

const initial: ResolvedRepos = { schema: "carrick.resolve-repos/0", workspace: { slug: "acme", billing_tier: "free", installed: false }, allowance_sentence: null, repos: [{ full_name: "acme/api", connected: false }], project_repos: [] };
const connected: ResolvedRepos = { ...initial, workspace: { ...initial.workspace, installed: true }, repos: [{ full_name: "acme/api", connected: true, project_id: "p1", project_slug: "system", services: [] }] };
const assignedToDefault: ResolvedRepos = { ...connected, repos: [{ full_name: "acme/api", connected: true, project_id: "p0", project_slug: "default-project", services: [] }], project_repos: [{ project_slug: "default-project", repos: ["acme/api"] }] };
const assignedToPayments: ResolvedRepos = { ...connected, repos: [{ full_name: "acme/api", connected: true, project_id: "p2", project_slug: "payments", services: [] }], project_repos: [{ project_slug: "payments", repos: ["acme/api"] }] };

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

test("a repo connected to another project remains pending until it reaches the requested project", async () => {
  const lines: string[] = [];
  let polls = 0;
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: true,
    project: "payments",
    say: (line) => lines.push(line),
    wait: async () => {},
    open: async (url) => {
      assert.equal(url, "https://app.carrick.tools/w/acme/projects");
      return true;
    },
    poll: async (signal) => resolveRepos(
      "token",
      ["acme/api"],
      async (_url, request) => {
        assert.deepEqual(JSON.parse(String(request?.body)), {
          action: "resolve-repos",
          repos: ["acme/api"],
        });
        return Response.json(++polls === 1 ? assignedToDefault : assignedToPayments);
      },
      signal,
    ),
  });

  assert.equal(polls, 2);
  assert.deepEqual(result, assignedToPayments);
  assert.match(lines.join("\n"), /acme\/api is currently in project "default-project"/);
  assert.match(lines.join("\n"), /Create project "payments" if needed: https:\/\/app\.carrick\.tools\/w\/acme\/projects/);
  assert.match(lines.join("\n"), /Assign the requested repos: https:\/\/app\.carrick\.tools\/w\/acme\/repos/);
  assert.match(lines.join("\n"), /Verified 1 repo in project "payments"/);
});

test("a wrong project cannot be replayed as though it were selected", async () => {
  const lines: string[] = [];
  const controller = new AbortController();
  let polls = 0;
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: true,
    project: "payments",
    signal: controller.signal,
    say: (line) => lines.push(line),
    open: async () => true,
    wait: async () => {},
    poll: async () => {
      polls += 1;
      controller.abort();
      return assignedToDefault;
    },
  });

  assert.equal(polls, 1);
  assert.deepEqual(result, assignedToDefault);
  assert.match(lines.at(-1) ?? "", /not verified/);
  assert.doesNotMatch(lines.join("\n"), /Verified 1 repo/);
});

test("project verification is non-vacuous and matches requested repository identities", () => {
  assert.equal(reposAreInProject(assignedToPayments, [], "payments"), false);
  assert.equal(reposAreInProject(assignedToPayments, ["acme/missing"], "payments"), false);
  assert.equal(reposAreInProject(assignedToPayments, ["ACME/API"], "payments"), true);
  assert.equal(reposAreInProject(assignedToDefault, ["acme/api"], "payments"), false);
});

test("an already verified repeated init neither opens nor polls", async () => {
  const lines: string[] = [];
  const result = await connectRepos("token", ["acme/api"], assignedToPayments, {
    interactive: true,
    project: "payments",
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
    poll: async () => { throw new Error("must not poll"); },
  });

  assert.deepEqual(result, assignedToPayments);
  assert.match(lines.join("\n"), /acme\/api is currently in project "payments"/);
  assert.match(lines.join("\n"), /Verified 1 repo in project "payments"/);
});

test("noninteractive project setup prints browser steps and returns unverified", async () => {
  const lines: string[] = [];
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: false,
    project: "payments",
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
    poll: async () => { throw new Error("must not poll"); },
  });

  assert.deepEqual(result, assignedToDefault);
  assert.match(lines.join("\n"), /Create project "payments" if needed/);
  assert.match(lines.at(-1) ?? "", /not verified/);
});

test("a polling network error aborts without a verification claim", async () => {
  const lines: string[] = [];
  await assert.rejects(
    connectRepos("token", ["acme/api"], assignedToDefault, {
      interactive: true,
      project: "payments",
      say: (line) => lines.push(line),
      open: async () => true,
      wait: async () => {},
      poll: async () => { throw new Error("network unavailable"); },
    }),
    /network unavailable/,
  );
  assert.doesNotMatch(lines.join("\n"), /Verified 1 repo/);
});

test("a project verification deadline ends clearly unverified", async () => {
  const lines: string[] = [];
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: true,
    project: "payments",
    signal: AbortSignal.timeout(5),
    say: (line) => lines.push(line),
    open: async () => true,
    wait: async () => { await new Promise((resolve) => setTimeout(resolve, 20)); },
    poll: async () => { throw new Error("must not poll after the deadline"); },
  });

  assert.deepEqual(result, assignedToDefault);
  assert.match(lines.at(-1) ?? "", /not verified/);
  assert.doesNotMatch(lines.join("\n"), /Verified 1 repo/);
});
