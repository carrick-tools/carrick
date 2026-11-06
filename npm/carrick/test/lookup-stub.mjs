// Preloaded into a hook process (`node --import`): replaces fetch so a test can
// see what the hook sends without a network. Behaviour comes from
// LOOKUP_STUB_MODE; the request body is appended to LOOKUP_STUB_LOG, a file the
// test keeps outside the HOME it hands the hook.
import fs from "node:fs";

globalThis.fetch = async (_url, init) => {
  fs.appendFileSync(
    process.env.LOOKUP_STUB_LOG,
    `${JSON.stringify({ body: JSON.parse(init.body), headers: init.headers })}\n`,
  );
  const mode = process.env.LOOKUP_STUB_MODE ?? "rows";
  if (mode === "slow") {
    await new Promise((resolve) => setTimeout(resolve, 3500));
  }
  if (mode === "401" || mode === "403" || mode === "500") {
    return new Response("no", { status: Number(mode) });
  }
  return Response.json({
    results: [
      { name: "parseAmount", file_path: "core/src/money.ts", line_number: 12 },
      { name: "formatAmount", file_path: "core/src/money.ts", line_number: 40 },
      { name: "sumAmounts", file_path: "billing/src/sum.ts", line_number: 7 },
      { name: "fourth", file_path: "x.ts", line_number: 1 },
    ],
  });
};
