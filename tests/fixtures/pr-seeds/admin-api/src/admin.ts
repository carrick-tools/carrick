import make, { Request, Response } from "fixture-server";

function listOrders(_request: Request, response: Response): void {
  response.send([]);
}

function cancelOrder(_request: Request, response: Response): void {
  response.send({ cancelled: true });
}

function report(_request: Request, response: Response): void {
  response.send({ rows: [] });
}

export const app = make();
app.get("/api/v1/admin/orders", listOrders);
app.post("/api/v1/admin/orders/:orderId/cancel", cancelOrder);

for (const period of ["daily", "weekly"] as const) {
  app.get(`/api/v1/admin/reports/${period}`, report);
}
