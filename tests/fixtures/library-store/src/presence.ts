import { Beacon } from "@fixture/beacon";

const beacon = new Beacon("wss://presence.example/ws");

beacon.on("presence", (message: unknown) => {
  console.log(message);
});
