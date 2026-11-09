import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
function pathOf(r: IncomingMessage) { return r.url; }
createServer((req, res) => {
  if (req.method === "GET" && pathOf(req) === "/s-helper-return") { res.end("a"); }
});
