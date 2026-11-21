import { callEdge } from "./callEdge";
import { useEdge } from "./useEdge";

export async function onSave() {
  await callEdge("send-invite", { email: "a@example.test" });
  await useEdge("report-export")({ month: 1 });
}
