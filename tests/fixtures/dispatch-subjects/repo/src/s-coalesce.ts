import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
const OVERRIDE: string | undefined = undefined;
createServer((req, res) => {
  const path = OVERRIDE ?? req.url;
  if (req.method === "GET" && path === "/s-coalesce") { res.end("a"); }
});
