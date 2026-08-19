import { getContext } from "@fixture/context-kit";

export const sendPrimary = async (to: string): Promise<void> => {
  const { transport } = await getContext(true);

  await transport.sendMail({ to });
};
