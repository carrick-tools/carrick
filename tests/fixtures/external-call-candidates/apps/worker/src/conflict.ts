import { shared, soloClient } from "@fixture/conflict-kit";

export const attempt = async (): Promise<void> => {
  await shared.send({ kind: "ambiguous" });
  await soloClient.upload("key", "body");
};
