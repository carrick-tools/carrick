// What `carrick init` does with a server that may not have the project
// actions yet (carrick#955).
//
// The case that decides the release is the first one: today's API answers an
// action it does not know with the credential-kind gate, which is a 403 and
// not a 404, and every one of those has to read as "fall back to the browser"
// rather than as a failure.

import assert from "node:assert/strict";
import test from "node:test";
import { listProjects, createProject, projectLines } from "../src/init/projects.ts";

const TOKEN = "test-token";

function answering(reply: (body: Record<string, unknown>) => Response): typeof fetch {
  return (async (input: string | URL | Request, init?: RequestInit) => {
    assert.equal(String(input), "https://api.carrick.tools/types/check-or-upload");
    assert.equal((init?.headers as Record<string, string>)["Authorization"], `Bearer ${TOKEN}`);
    return reply(JSON.parse(String(init?.body)) as Record<string, unknown>);
  }) as typeof fetch;
}

const KIND_GATE = Response.json(
  { error: "MCP keys cannot authenticate scan traffic." },
  { status: 403 },
);

test("the deployed server's answer to an action it has never heard of is a fallback", async () => {
  const request = answering(() => KIND_GATE);
  assert.equal(await listProjects(TOKEN, request), null);
  assert.deepEqual(await createProject(TOKEN, "payments", "payments", request), { kind: "absent" });
});

test("a 404, an unreadable body and an unknown schema are all absences", async () => {
  for (const reply of [
    () => new Response("", { status: 404 }),
    () => new Response("not json", { status: 200 }),
    () => Response.json({ schema: "carrick.list-projects/1", projects: [] }),
    () => Response.json({ projects: [{ slug: "payments" }] }),
  ]) {
    assert.equal(await listProjects(TOKEN, answering(reply)), null);
  }
  for (const reply of [
    () => new Response("", { status: 404 }),
    () => new Response("not json", { status: 200 }),
    () => Response.json({ schema: "carrick.create-project/1", project: {} }),
  ]) {
    assert.deepEqual(await createProject(TOKEN, "payments", "payments", answering(reply)), {
      kind: "absent",
    });
  }
});

test("a rejected credential is never read as a missing feature", async () => {
  const request = answering(() => Response.json({ error: "no" }, { status: 401 }));
  await assert.rejects(listProjects(TOKEN, request), /Run carrick login/);
  await assert.rejects(createProject(TOKEN, "payments", "payments", request), /Run carrick login/);
});

test("an unreachable API falls back rather than failing the run", async () => {
  const request = (async () => {
    throw new Error("offline");
  }) as typeof fetch;
  assert.equal(await listProjects(TOKEN, request), null);
  assert.deepEqual(await createProject(TOKEN, "payments", "payments", request), { kind: "absent" });
});

test("the workspace's projects come back when the action exists", async () => {
  const projects = await listProjects(
    TOKEN,
    answering((body) => {
      assert.equal(body["action"], "list-projects");
      return Response.json({
        schema: "carrick.list-projects/0",
        projects: [
          { slug: "default", name: "Default", archived: false, repo_count: 3 },
          { slug: "old", name: "Old", archived: true, repo_count: 0 },
        ],
      });
    }),
  );
  assert.equal(projects?.length, 2);
  assert.deepEqual(projectLines(projects!), [
    "  default  Default  3 repos",
    "  old  Old  0 repos  (archived)",
  ]);
});

test("a create sends the slug it was given, and reports what came back", async () => {
  const created = await createProject(
    TOKEN,
    "payments",
    "Payments",
    answering((body) => {
      assert.equal(body["action"], "create-project");
      assert.equal(body["slug"], "payments");
      assert.equal(body["name"], "Payments");
      return Response.json({
        schema: "carrick.create-project/0",
        project: { slug: "payments", name: "Payments", archived: false, repo_count: 0 },
      });
    }),
  );
  assert.equal(created.kind, "created");
  assert.equal(created.kind === "created" ? created.project.slug : null, "payments");
});

// A refusal is the server stating a rule. It is the one non-2xx that must not
// read as an absence, because falling back would hide the reason.
test("a slug the server refuses is reported with its reason", async () => {
  for (const status of [400, 409]) {
    const outcome = await createProject(
      TOKEN,
      "payments",
      "payments",
      answering(() => Response.json({ error: "That slug is taken." }, { status })),
    );
    assert.deepEqual(outcome, { kind: "refused", message: "That slug is taken." });
  }
  // A refusal status with nothing to say is not a sentence worth printing.
  assert.deepEqual(
    await createProject(
      TOKEN,
      "payments",
      "payments",
      answering(() => new Response("", { status: 409 })),
    ),
    { kind: "absent" },
  );
});

test("the list prints active projects before archived ones", () => {
  const lines = projectLines([
    { slug: "zeta", name: "zeta", archived: false, repo_count: null },
    { slug: "alpha", name: "alpha", archived: true, repo_count: 1 },
    { slug: "beta", name: "beta", archived: false, repo_count: 1 },
  ]);
  assert.deepEqual(lines, ["  beta  1 repo", "  zeta", "  alpha  1 repo  (archived)"]);
});
