import { sendNotice } from "courier-sdk";

export const shout = (): Promise<void> => sendNotice({ userId: "orphan" });
