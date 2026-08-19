import digestTransport from "@fixture/mail-kit/digest";

export const sendDigest = async (to: string): Promise<void> => {
  const transport = digestTransport();

  await transport.sendMail({ to });
};
