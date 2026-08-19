import { getContext } from "@fixture/context-kit";

export const sendFallback = async (to: string): Promise<void> => {
  const context = await getContext(false);

  await context.transport.sendMail({ to });
};
