import { mailer, verifyTransport } from "@fixture/mail-kit";

export const notify = async (to: string): Promise<void> => {
  await mailer.sendMail({ to });
  verifyTransport();
};
