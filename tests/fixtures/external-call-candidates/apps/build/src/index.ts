import { sendNotice } from "courier-sdk";

export const announce = (userId: string): Promise<void> => sendNotice({ userId });
