// An ES module: `import`/`export`, the HTTP route, and a call into the
// CommonJS module beside it.
import { createServer } from "node:http";
import { priceOrder } from "./pricing.cjs";

export function handleOrder(request, response) {
  const total = priceOrder([{ unitPrice: 250, quantity: 2 }]);
  response.writeHead(200, { "content-type": "application/json" });
  response.end(JSON.stringify({ total }));
}

export function startServer(port) {
  return createServer((request, response) => {
    if (request.method === "POST" && request.url === "/orders") {
      return handleOrder(request, response);
    }
    response.writeHead(404);
    response.end();
  }).listen(port);
}
