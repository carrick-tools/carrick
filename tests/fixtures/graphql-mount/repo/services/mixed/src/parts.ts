import type { Schema } from "fixture-gql-server";

export function buildParts(): { schema: Schema } {
  return { schema: { kind: "Schema", types: ["Query"] } };
}
