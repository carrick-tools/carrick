import { Live } from "@fixture/live-wire";

export function announce(s: Live, message: string): void {
  s.emit("chat.sent", message);
}
