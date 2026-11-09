import { createServer } from "node:http";
import type { IncomingMessage, ServerResponse } from "node:http";
createServer((req, res) => {
  const path = (req.url ?? "/").split("?")[0];
  if (req.method === "GET" && path === "/s-split") { res.end("a"); }
});
