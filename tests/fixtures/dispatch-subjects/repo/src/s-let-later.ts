import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  let path;
  path = req.url;
  if (req.method === "GET" && path === "/s-let-later") { res.end("a"); }
});
