import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  let path = req.url ?? "/";
  if (path.endsWith("/")) path = path.slice(0, -1);
  if (req.method === "GET" && path === "/s-let-normalised") { res.end("a"); }
});
