import { getSplitContext } from "@fixture/context-kit/split";

export const sendSplit = async (to: string): Promise<void> => {
  const { transport } = getSplitContext(true);

  await transport.sendMail({ to });
};
