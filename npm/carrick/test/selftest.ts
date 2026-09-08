#!/usr/bin/env node
// One channel per install, counted.
//
// The hook and the language server carry the same verdicts, so an install that
// delivers both gives the model every finding twice and makes a measurement of
// either channel a measurement of the pair. That is the arm-health rule from
// the spike retro, and this is it as a check: for each install shape, drive one
// edit through both entry points and count the hook contexts and the
// `publishDiagnostics` notifications separately.
//
// It is the unit analogue of the transcript count. The live version, over a
// real session, is the grader in SMOKE.md; this one proves the wiring with the
// fake CLI and no model at all.
//
// Run: npm run selftest

import { LspClient } from "./lsp-client.ts";
import { editPayload, fakeEnv, makeWorkspace, runHook } from "./helpers.ts";

export type Shape = {
  name: string;
  /** What the install registers: the plugin ships both, an editor has no hook. */
  hookInstalled: boolean;
  serverArgs: string[];
  env: Record<string, string>;
  expect: { hookContexts: number; diagnosticAttachments: number };
};

export const SHAPES: Shape[] = [
  {
    name: "Claude Code plugin",
    hookInstalled: true,
    serverArgs: ["--hooks-installed"],
    env: {},
    expect: { hookContexts: 1, diagnosticAttachments: 0 },
  },
  {
    name: "editor or generic LSP client",
    hookInstalled: false,
    serverArgs: [],
    env: {},
    expect: { hookContexts: 0, diagnosticAttachments: 1 },
  },
  {
    name: "plugin, LSP arm pinned",
    hookInstalled: true,
    serverArgs: ["--hooks-installed"],
    env: { CARRICK_CHANNEL: "lsp" },
    expect: { hookContexts: 0, diagnosticAttachments: 1 },
  },
  {
    name: "editor, hook arm pinned",
    hookInstalled: true,
    serverArgs: [],
    env: { CARRICK_CHANNEL: "hook" },
    expect: { hookContexts: 1, diagnosticAttachments: 0 },
  },
  {
    name: "silenced",
    hookInstalled: true,
    serverArgs: ["--hooks-installed"],
    env: { CARRICK_CHANNEL: "off" },
    expect: { hookContexts: 0, diagnosticAttachments: 0 },
  },
];

export type Counts = { hookContexts: number; diagnosticAttachments: number };

export async function measure(shape: Shape): Promise<Counts> {
  const workspace = makeWorkspace();
  const env = fakeEnv(shape.env);
  let hookContexts = 0;
  const client = new LspClient({ args: shape.serverArgs, env });
  try {
    if (shape.hookInstalled) {
      const run = await runHook("post-edit.ts", { payload: editPayload(workspace), env });
      if (run.stdout.includes("additionalContext")) hookContexts += 1;
    }
    await client.initialize(workspace.root);
    client.open(workspace.file);
    await client.settle(700);
    // One edit, one delivery per channel: a check that publishes to the edited
    // file and to three counterpart files is still one attachment for the model.
    const published = client.publishes.some((publish) => publish.diagnostics.length > 0);
    return { hookContexts, diagnosticAttachments: published ? 1 : 0 };
  } finally {
    client.stop();
    workspace.cleanup();
  }
}

export async function run(): Promise<{ rows: Array<Shape & Counts>; failures: number }> {
  const rows: Array<Shape & Counts> = [];
  let failures = 0;
  for (const shape of SHAPES) {
    const counts = await measure(shape);
    if (
      counts.hookContexts !== shape.expect.hookContexts ||
      counts.diagnosticAttachments !== shape.expect.diagnosticAttachments
    ) {
      failures += 1;
    }
    rows.push({ ...shape, ...counts });
  }
  return { rows, failures };
}

function table(rows: Array<Shape & Counts>): string {
  const header = ["install", "hook contexts", "diagnostic attachments", "expected"];
  const body = rows.map((row) => [
    row.name,
    String(row.hookContexts),
    String(row.diagnosticAttachments),
    `${row.expect.hookContexts}/${row.expect.diagnosticAttachments}`,
  ]);
  const widths = header.map((cell, column) =>
    Math.max(cell.length, ...body.map((line) => (line[column] ?? "").length)),
  );
  const line = (cells: string[]): string =>
    cells.map((cell, column) => cell.padEnd(widths[column] ?? 0)).join("  ");
  return [line(header), line(widths.map((width) => "-".repeat(width))), ...body.map(line)].join("\n");
}

if (import.meta.filename === process.argv[1]) {
  const { rows, failures } = await run();
  process.stdout.write(`${table(rows)}\n`);
  if (failures > 0) {
    process.stdout.write(`${failures} install shape(s) delivered on the wrong channel\n`);
    process.exit(1);
  }
  process.stdout.write("one channel per install\n");
}
