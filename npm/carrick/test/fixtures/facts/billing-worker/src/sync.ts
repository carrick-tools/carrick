import { client } from "./client";

// Runs once at start-up: no function encloses this call.
const order = await client.get("/orders/:id", { params: { id: process.env["ORDER_ID"] } });
console.log(order.status);
