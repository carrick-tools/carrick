// The repo's environment schema. It defines no route and makes no request, so
// it raises no candidate and the analyzer never sees it — the declarations below
// are visible only because the schema read is repo-wide.
import { z } from "zod";

export const envSchema = z.object({
  // Optional with no default: the source says outright this may be absent at
  // runtime with nothing standing in for it.
  KNOWLEDGE_URL: z.string().url().optional(),
  // A default: never absent, whatever the environment supplies.
  GATEWAY_URL: z.string().url().default("https://api.example.com"),
});

export const env = envSchema.parse(process.env);
