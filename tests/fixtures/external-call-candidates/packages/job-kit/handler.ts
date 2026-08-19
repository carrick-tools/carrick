import { mailer } from "@fixture/mail-kit";

export const run = async (): Promise<void> => {
  await mailer.sendMail({ subject: "job finished" });
};
