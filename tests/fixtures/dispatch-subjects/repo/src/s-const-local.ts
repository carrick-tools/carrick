import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  const path = req.url;
  if (req.method === "GET" && path === "/s-const-local") { res.end("a"); }
});
