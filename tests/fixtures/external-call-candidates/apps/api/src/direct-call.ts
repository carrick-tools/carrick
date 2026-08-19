import { sendNotice } from "courier-sdk";

export async function notify(userId: string): Promise<void> {
  await sendNotice({ userId });
}
