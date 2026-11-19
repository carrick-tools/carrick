import { defineFn } from "@fixture/rpc-core";

export const grouped = {
  first: defineFn().handler(async () => 1),
  second: defineFn({ method: "POST" }).handler(async () => 2),
};
