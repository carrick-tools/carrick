import { io } from "@fixture/rt-client";
import type { OrderUpdate } from "./types";

const socket = io("https://orders.internal");
const update: OrderUpdate = { id: "o1", status: "open" };
socket.emit("order:update", update);
