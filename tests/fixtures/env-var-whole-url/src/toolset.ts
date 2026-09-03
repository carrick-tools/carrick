// The same whole-URL environment variable, at a site the extraction returns no
// row for at all: the request sits in an arrow function that is a property of
// an object literal handed to a factory call. Nothing in this file states a
// path outside the fallback literal, so there is nothing for the model to
// answer with and nothing for a rewrite to correct.
const askUrl = process.env.SERVICE_ASK_URL ?? "http://localhost:3939/api/ask";

export function buildToolset(defineTool: (spec: unknown) => unknown) {
  return defineTool({
    name: "ask",
    execute: async (question: string) => {
      const res = await fetch(askUrl, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ question }),
      });
      return res.text();
    },
  });
}
