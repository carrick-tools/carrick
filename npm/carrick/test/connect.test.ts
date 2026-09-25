import assert from "node:assert/strict";
import test from "node:test";
import { ADMIN_WAIT, connectRepos, reposAreInProject } from "../src/init/connect.ts";
import { resolveRepos, type ResolvedRepos } from "../src/auth/read.ts";
import type { AssignOutcome } from "../src/init/projects.ts";

/** One repo placed, as the server reports a write it made. */
function moved(repos: string[], project: string): AssignOutcome {
  return {
    kind: "placed",
    repos: repos.map((full_name) => ({ full_name, assigned: true, moved: true, project_slug: project, reason: null })),
  };
}

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
  // One status line while it waits, and the outcome; no line per repo
  // (carrick#1489).
  assert.ok(lines.includes("Waiting for GitHub: 0 of 1 repo connected. Press Ctrl-C to stop."), lines.join("\n"));
  assert.equal(lines.at(-1), "1 repo connected");
  assert.equal(lines.filter((line) => line.includes("acme/api")).length, 1, lines.join("\n"));
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
    movable: new Set(["acme/api"]),
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
  assert.match(lines.join("\n"), /Create project "payments" if needed: https:\/\/app\.carrick\.tools\/w\/acme\/projects/);
  assert.match(lines.join("\n"), /Assign the requested repos: https:\/\/app\.carrick\.tools\/w\/acme\/repos/);
  // The settled assignment is the claim, read back from the poll, and it is
  // the outcome line of the wait rather than a line per repo (carrick#1489).
  assert.equal(lines.at(-1), "1 repo connected, in payments");
  assert.doesNotMatch(lines.join("\n"), /currently in project/);
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
  // And says nothing at all: a repo already where it was asked to be is not a
  // thing that happened, and init states the project once, on the line that
  // names the login (carrick#1026).
  assert.deepEqual(lines, []);
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
    movable: new Set(["acme/api"]),
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

// carrick#999. The GitHub App grant puts a newly connected repo in the
// workspace's default project, so a run started with --project used to sit in
// this loop until a human opened the Repos page. Assignment is a server action
// on this credential now, and the browser keeps the grant and nothing else.
test("the requested repos are placed from here, and the claim still comes from the read", async () => {
  const lines: string[] = [];
  const asked: string[][] = [];
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: false,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
    assign: async (repos) => { asked.push(repos); return moved(repos, "payments"); },
    poll: async () => assignedToPayments,
  });

  assert.deepEqual(asked, [["acme/api"]]);
  assert.deepEqual(result, assignedToPayments);
  // A move out of a project nobody chose (the App grant's default) is not
  // announced (carrick#1489 part 3.3).
  assert.deepEqual(lines, []);
  // The browser step this replaces is not printed at all.
  assert.doesNotMatch(lines.join("\n"), /Assign the requested repos/);
  assert.doesNotMatch(lines.join("\n"), /not verified/);
});

// carrick#1338. A repo whose move nobody accepted is left where it is, and
// the browser step is printed instead. The consent is collected with the
// proposal, before this is ever called.
test("a repo nobody consented to move is not moved, and its project is not claimed", async () => {
  const lines: string[] = [];
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: false,
    project: "payments",
    projectExists: true,
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
    assign: async () => { throw new Error("must not ask the server to move a repo nobody accepted"); },
    poll: async () => { throw new Error("must not poll: nothing moved"); },
  });

  assert.deepEqual(result, assignedToDefault);
  // Left where it is, and said so: the project this run was asked for is not
  // claimed for a repo it did not place.
  assert.match(lines.at(-1) ?? "", /Project payments is not verified/);
});

test("a repo the server could not place is named, and the project is not claimed", async () => {
  const lines: string[] = [];
  const result = await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: false,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
    assign: async () => ({
      kind: "placed",
      repos: [{ full_name: "acme/api", assigned: false, moved: false, project_slug: null, reason: "could not be moved just now. Try again, or move it in the dashboard." }],
    }),
    poll: async () => { throw new Error("must not poll: nothing moved"); },
  });

  assert.deepEqual(result, assignedToDefault);
  assert.match(lines.join("\n"), /acme\/api was not moved: could not be moved just now\./);
  assert.match(lines.at(-1) ?? "", /not verified/);
  assert.doesNotMatch(lines.join("\n"), /Verified 1 repo/);
});

test("an API without the action falls back to the browser instruction, once", async () => {
  const lines: string[] = [];
  let attempts = 0;
  const controller = new AbortController();
  await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: true,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    signal: controller.signal,
    say: (line) => lines.push(line),
    open: async (url) => {
      // Everything is connected and the project exists, so the only page left
      // is the one that moves repos.
      assert.equal(url, "https://app.carrick.tools/w/acme/repos");
      return true;
    },
    wait: async () => {},
    poll: async () => { controller.abort(); return assignedToDefault; },
    assign: async () => { attempts += 1; return { kind: "absent" }; },
  });

  // Asked once. An absence is not a thing to retry every five seconds.
  assert.equal(attempts, 1);
  assert.equal(lines.filter((line) => line.startsWith("Assign the requested repos")).length, 1);
  assert.match(lines.join("\n"), /Move selected to/);
});

test("a refusal is printed in the server's own words before the browser line", async () => {
  const lines: string[] = [];
  await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: false,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
    assign: async () => ({ kind: "refused", message: 'project "payments" is archived. Unarchive it before moving repos into it.' }),
    poll: async () => { throw new Error("must not poll"); },
  });

  const said = lines.join("\n");
  assert.match(said, /Carrick did not assign the requested repos to payments: project "payments" is archived\./);
  assert.ok(said.indexOf("Carrick did not assign") < said.indexOf("Assign the requested repos"), said);
});

// The case the ticket is about: nothing is connected yet, so there is nothing
// to assign until the grant lands. The CLI waits on the grant, then places the
// repo itself.
test("assignment happens when the poll sees the repo, not before", async () => {
  const lines: string[] = [];
  const asked: string[][] = [];
  let polls = 0;
  const result = await connectRepos("token", ["acme/api"], initial, {
    interactive: true,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    say: (line) => lines.push(line),
    open: async (url) => {
      // Nothing is connected, so the browser is pointed at the grant.
      assert.equal(url, "https://app.carrick.tools/w/acme/connect");
      return true;
    },
    wait: async () => {},
    assign: async (repos) => { asked.push(repos); return moved(repos, "payments"); },
    poll: async () => { polls += 1; return polls === 1 ? assignedToDefault : assignedToPayments; },
  });

  assert.deepEqual(asked, [["acme/api"]]);
  assert.deepEqual(result, assignedToPayments);
  assert.equal(lines.at(-1), "1 repo connected, in payments");
  assert.doesNotMatch(lines.join("\n"), /Moved|currently in project/);
  assert.doesNotMatch(lines.join("\n"), /Assign the requested repos/);
});

// carrick#1489 review: the silence is for a move out of an untouched default.
// A repo the reader moved out of a project somebody chose gets its one line,
// naming both projects.
test("a consented move out of a chosen project is said, once", async () => {
  const lines: string[] = [];
  await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: false,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    announce: new Set(["acme/api"]),
    label: (slug) => (slug === "payments" ? "Payments (payments)" : `Acme Default (${slug})`),
    say: (line) => lines.push(line),
    assign: async (repos) => moved(repos, "payments"),
    poll: async () => assignedToPayments,
  });
  assert.deepEqual(lines, ["Moved acme/api from Acme Default (default-project) into Payments (payments)."]);
});

// carrick#1489 review: the owner-or-admin sentence is about a browser step,
// so a wait on placements this run makes itself does not print it.
test("a wait with no browser step does not say who can finish one", async () => {
  const lines: string[] = [];
  const opened: string[] = [];
  const controller = new AbortController();
  await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: true,
    project: "payments",
    projectExists: true,
    movable: new Set(["acme/api"]),
    signal: controller.signal,
    say: (line) => lines.push(line),
    open: async (url) => { opened.push(url); return true; },
    // Placed, but the read has not caught up yet.
    assign: async (repos) => moved(repos, "payments"),
    poll: async () => assignedToDefault,
    wait: async () => { controller.abort(); },
  });
  assert.ok(!lines.includes(ADMIN_WAIT), lines.join("\n"));
  assert.deepEqual(opened, []);
});

// carrick#1489 part 3.1. The grant page re-asks GitHub for the whole repo
// list, so a repo the read taken just now calls connected is not sent to it,
// and a wait with nothing unconnected opens no grant page at all.
test("only a repo still unconnected is sent to the GitHub App", async () => {
  const half: ResolvedRepos = {
    ...connected,
    repos: [
      { full_name: "acme/api", connected: true, project_id: "p1", project_slug: "system", services: [] },
      { full_name: "acme/web", connected: false },
    ],
  };
  const lines: string[] = [];
  await connectRepos("token", ["acme/api", "acme/web"], half, {
    interactive: false,
    say: (line) => lines.push(line),
    open: async () => { throw new Error("must not open"); },
  });
  assert.deepEqual(lines, [
    "acme/web is not connected yet. Install the Carrick GitHub App on it: https://app.carrick.tools/w/acme/connect",
  ]);

  const both: string[] = [];
  await connectRepos("token", ["acme/api", "acme/web"], { ...initial, repos: [] }, {
    interactive: false,
    say: (line) => both.push(line),
  });
  assert.deepEqual(both, [
    "Neither repo is connected yet. Install the Carrick GitHub App on both: https://app.carrick.tools/w/acme/connect",
  ]);

  // Connected, and only the move left: the grant page is not opened.
  const opened: string[] = [];
  const controller = new AbortController();
  await connectRepos("token", ["acme/api"], assignedToDefault, {
    interactive: true,
    project: "payments",
    projectExists: true,
    signal: controller.signal,
    say: () => {},
    open: async (url) => { opened.push(url); return true; },
    assign: async () => ({ kind: "absent" }),
    wait: async () => { controller.abort(); },
  });
  assert.deepEqual(opened, ["https://app.carrick.tools/w/acme/repos"]);
});

// carrick#1489 part 3.2. Four lines per repo per change was the output; the
// wait is now one status line that changes only when the count does, and a
// repo gets a line of its own only when something about it failed.
test("the wait is one status line, and a failure is the only per-repo line", async () => {
  const two = ["acme/api", "acme/web"];
  const none: ResolvedRepos = { ...connected, repos: two.map((full_name) => ({ full_name, connected: false })) };
  const one: ResolvedRepos = {
    ...connected,
    repos: [
      { full_name: "acme/api", connected: true, project_id: "p0", project_slug: "default", services: [] },
      { full_name: "acme/web", connected: false },
    ],
  };
  const reads = [none, none, none, one, one];
  const shown: string[] = [];
  const lines: string[] = [];
  const controller = new AbortController();
  let polls = 0;
  await connectRepos("token", two, none, {
    interactive: true,
    project: "payments",
    projectExists: true,
    movable: new Set(two),
    signal: controller.signal,
    say: (line) => lines.push(line),
    track: async (label, work, report) => {
      shown.push(label);
      const value = await work((text) => shown.push(text));
      shown.push(report(value).text);
      return value;
    },
    open: async () => true,
    wait: async () => {},
    assign: async () => ({
      kind: "placed",
      repos: [{ full_name: "acme/api", assigned: false, moved: false, project_slug: null, reason: "could not be moved just now." }],
    }),
    poll: async () => {
      const read = reads[polls] ?? one;
      polls += 1;
      if (polls >= reads.length) controller.abort();
      return read;
    },
  });
  assert.deepEqual(shown, [
    "Waiting for GitHub: 0 of 2 repos connected",
    "Waiting for GitHub: 0 of 2 repos connected. Press Ctrl-C to stop.",
    "Waiting for GitHub: 1 of 2 repos connected. Press Ctrl-C to stop.",
    "Stopped waiting; project payments is not verified for every requested repo.",
  ]);
  // Said once, and after the status line let go of the terminal.
  assert.deepEqual(lines.filter((line) => line.includes("was not moved")), [
    "acme/api was not moved: could not be moved just now.",
  ]);
  assert.ok(lines.indexOf("acme/api was not moved: could not be moved just now.") > lines.indexOf(ADMIN_WAIT));
});

// carrick#993 row 6. Both pages this waits on refuse anyone who is not an
// owner or an admin of the workspace, and the wait is thirty minutes long.
test("every wait says who can end it, before it starts", async () => {
  for (const project of [undefined, "payments"]) {
    const lines: string[] = [];
    const controller = new AbortController();
    await connectRepos("token", ["acme/api"], project === undefined ? initial : assignedToDefault, {
      interactive: true,
      project,
      signal: controller.signal,
      say: (line) => lines.push(line),
      open: async () => true,
      wait: async () => { controller.abort(); },
      poll: async () => { throw new Error("must not poll after cancellation"); },
    });
    const waiting = lines.findIndex((line) => line.startsWith("Waiting"));
    assert.ok(waiting > 0, lines.join("\n"));
    assert.equal(lines[waiting - 1], ADMIN_WAIT);
  }
});
