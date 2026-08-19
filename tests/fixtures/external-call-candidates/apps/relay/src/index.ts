import { mailer } from "@fixture/mail-kit";

export const relay = async (to: string): Promise<void> => {
  await mailer.sendMail({ to }).catch(() => undefined);
};
