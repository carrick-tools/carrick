import { sendNotice } from "courier-sdk";

it("notifies", async () => {
  await sendNotice({ userId: "u1" });
});
