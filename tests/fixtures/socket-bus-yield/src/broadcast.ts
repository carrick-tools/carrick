import { Live } from "@fixture/live-wire";

export function broadcast(hub: Live, room: string, message: string): void {
  hub
    .to(room)
    .emit("chat.multi", message);
}
