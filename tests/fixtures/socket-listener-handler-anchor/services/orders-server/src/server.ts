import { Server } from "@fixture/rt-server";
import { bus } from "@fixture/queue";
import { createServer } from "http";
import type { OrderEvents, OrderUpdate } from "./types";

const httpServer = createServer();
const io = new Server<OrderEvents>(httpServer);

function handleUpdate({ id, status }: OrderUpdate): void {
  console.log(id, status);
}

const notAFunction: any = 1;

io.on("connection", (socket) => {
  // destructured parameter, annotated
  socket.on("order:update", async ({ id, status }: OrderUpdate) => {
    console.log(id, status);
  });
  // named parameter, typed only by the server's event map
  socket.on("order:ping", async (message) => {
    console.log(message.id);
  });
  // any-typed parameter
  socket.on("order:any", (event: any) => {
    console.log(event);
  });
  // two function arguments beside the event name
  socket.once(
    "order:twice",
    (first: OrderUpdate) => console.log(first.id),
    (second: OrderUpdate) => console.log(second.id),
  );
  // no event name: the first parameter is the event name
  socket.onAny((event: string, payload: OrderUpdate) => {
    console.log(event, payload.id);
  });
  // a name the library emits itself
  socket.on("disconnect", (reason: OrderUpdate) => {
    console.log(reason);
  });
  // a function declared elsewhere in the module
  socket.on("order:named", handleUpdate);
  // an identifier that is not a function
  socket.on("order:notfn", notAFunction);
  // a member whose label states its handler slot
  socket.listen("order:channel", ({ id, status }: OrderUpdate) => {
    console.log(id, status);
  });
});

// a broker receive: untouched
bus.subscribe("order:bus", (e: OrderUpdate) => {
  console.log(e.id);
});

httpServer.listen(4000);
