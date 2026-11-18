import make, { Request, Response } from "fixture-server";

function listTickets(_request: Request, response: Response): void {
  response.send([]);
}

export const app = make();
app.get("/api/v1/desk/tickets", listTickets);
