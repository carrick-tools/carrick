import make, { Request, Response } from "fixture-server";

interface Order {
  id: string;
  qty: number;
}

function saveOrder(request: Request, response: Response): void {
  const order = request.body as Order;
  response.send({ saved: order.id });
}

function untypedBody(request: Request, response: Response): void {
  const body = request.body;
  response.send({ seen: body.id });
}

function pathOnly(request: Request, response: Response): void {
  response.send({ at: request.path });
}

export const app = make();
app.post("/orders", saveOrder);
app.post("/untyped", untypedBody);
app.post("/path-only", pathOnly);
