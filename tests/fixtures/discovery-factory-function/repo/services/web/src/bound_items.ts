import { makeBound } from "./bound";

async function stub(_url: string): Promise<Response> {
  return new Response("{}");
}

export async function bound(): Promise<unknown> {
  const send = makeBound({ fetch: stub });
  return send({ path: "/v1/bound", fetch: globalThis.fetch });
}
