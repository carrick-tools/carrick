import { tasks } from "@fixture/tasks";
import { sendEmail } from "./tasks";

export async function onSignup(email: string) {
  await tasks.trigger("send-email", { to: email });
  await sendEmail.triggerAndWait({ to: email });
}

export async function byName(name: string) {
  await tasks.trigger(name, {});
  await tasks.trigger(`report-${name}`, {});
}

const REPORT = "parent-task";

export async function shadowed() {
  const REPORT = "not-a-task";
  await tasks.trigger(REPORT, {});
}

export async function moduleConstant() {
  await tasks.trigger(REPORT, {});
}
