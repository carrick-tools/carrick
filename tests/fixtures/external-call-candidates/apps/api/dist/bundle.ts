import { sendNotice } from "courier-sdk";

export function bundled(): void {
  sendNotice({ userId: "generated" });
}
