// The same whole-URL environment variable at a site the extraction DOES answer
// for, paraphrasing the binding as the bare env-var name. The row is the call,
// but the target it states is not the spelling the pipeline resolves env vars
// through, and it carries nothing about the origin the source defaults to.
const knowledgeUrl = process.env.KNOWLEDGE_URL ?? "http://localhost:3939/api/lookup";

export function buildKnowledgeTool(defineTool: (spec: unknown) => unknown) {
  return defineTool({
    name: "look_up",
    execute: async (question: string) => {
      const res = await fetch(knowledgeUrl, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ question }),
      });
      return res.text();
    },
  });
}
