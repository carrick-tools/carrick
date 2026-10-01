const ticketUrl = process.env.TICKETS_URL ?? "http://localhost:7200/api/tickets";

export async function openTicket() {
  return fetch(ticketUrl, { method: "POST" });
}
