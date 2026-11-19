import { defineFn } from "@fixture/rpc-core";

export const listNotes = defineFn({ method: "GET" }).handler(async () => [{ id: 1 }]);

export const saveNote = defineFn({ method: "POST" })
  .use([])
  .validate((d: { body: string }) => d)
  .handler(async ({ data }) => ({ ok: true }));

export const unused = defineFn().handler(async () => 1);
