import { Live } from "@fixture/live-wire";

export function listen(s: Live): void {
  s.on("chat.read", (receipt: unknown) => {
    console.log(receipt);
  });
}
