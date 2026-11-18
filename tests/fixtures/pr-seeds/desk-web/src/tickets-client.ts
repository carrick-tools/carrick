import { send } from "./send.js";

export type Ticket = { id: string; title: string };

export class TicketsClient {
  constructor(private readonly baseUrl: string) {}

  listTickets(): Promise<Ticket[]> {
    return send<Ticket[]>(`${this.baseUrl}/api/v1/desk/tickets`, {
      method: "GET",
    });
  }
}
