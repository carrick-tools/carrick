// The workspace's projects, read so init prints each one as the dashboard
// shows it (carrick#955, carrick#1338). Read only: projects are created and
// filled on the dashboard (carrick#3361).
//
// The case that matters most is the first one: an API that has never heard of
// the action answers the credential-kind gate, which is a 403 and not a 404,
// and every one of those has to read as "slugs alone" rather than as a
// failure.

import assert from "node:assert/strict";
import test from "node:test";
import { listProjects, projectLabel } from "../src/init/projects.ts";

const TOKEN = "test-token";

function answering(reply: (body: Record<string, unknown>) => Response): typeof fetch {
  return (async (input: string | URL | Request, init?: RequestInit) => {
    assert.equal(String(input), "https://api.carrick.tools/types/check-or-upload");
    assert.equal((init?.headers as Record<string, string>)["Authorization"], `Bearer ${TOKEN}`);
    return reply(JSON.parse(String(init?.body)) as Record<string, unknown>);
  }) as typeof fetch;
}

test("the deployed server's answer to an action it has never heard of is a fallback", async () => {
  const request = answering(() => Response.json({ error: "MCP keys cannot authenticate scan traffic." }, { status: 403 }));
  assert.equal(await listProjects(TOKEN, request), null);
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
});

test("a rejected credential is never read as a missing feature", async () => {
  const request = answering(() => Response.json({ error: "no" }, { status: 401 }));
  await assert.rejects(listProjects(TOKEN, request), /Run carrick login/);
});

test("an unreachable API falls back rather than failing the run", async () => {
  const request = (async () => {
    throw new Error("offline");
  }) as typeof fetch;
  assert.equal(await listProjects(TOKEN, request), null);
});

test("the workspace's projects come back when the action exists, and print by name and slug", async () => {
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
  // Display name and slug: the dashboard shows the name, every CLI line
  // showed the slug, and nothing connected the two (carrick#1338).
  assert.equal(projectLabel("default", projects!), "Default (default)");
  // A project whose display name IS its slug is printed once, and a slug the
  // list does not hold, or no list at all, is the slug alone.
  assert.equal(projectLabel("payments", [{ slug: "payments", name: "payments", archived: false, repo_count: 1 }]), "payments");
  assert.equal(projectLabel("missing", projects!), "missing");
  assert.equal(projectLabel("default", null), "default");
});
