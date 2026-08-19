import { backupMailer, mailer } from "@fixture/mail-kit";

export const getContext = async (primary: boolean) => {
  if (primary) {
    return { transport: mailer, region: "eu-west" };
  }

  return { transport: primary ? mailer : backupMailer, region: "us-east" };
};
