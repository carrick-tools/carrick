import { mailer } from "@fixture/mail-kit";

export const local = (): string => {
  const mailer = { sendMail: (to: string) => to };

  return mailer.sendMail("nobody");
};

export const real = (to: string): Promise<void> => mailer.sendMail({ to });
