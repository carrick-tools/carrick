import { backupMailer, mailer } from "@fixture/mail-kit";

export const getSplitContext = (primary: boolean) =>
  primary
    ? { transport: mailer, region: "eu-central" }
    : { transport: backupMailer, region: "us-west" };
