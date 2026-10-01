import { job, task } from "@fixture/tasks";

export const sendEmail = task({
  id: "send-email",
  run: async (payload: { to: string }) => {
    console.log(payload.to);
  },
});

const CHILD_ID = "child-task";

export const childTask = task({
  id: CHILD_ID,
  run: async () => {},
});

export const parentTask = task({
  id: "parent-task",
  run: async () => {
    await childTask.trigger();
    await sendEmail.trigger({ to: "ops@example.com" });
  },
});

// Two string keys: which one names the job is not in the types.
export const nightly = job({
  name: "nightly-report",
  queue: "reports",
  run: async () => {},
});

const dynamicId = process.env.TASK_ID ?? "fallback-task";

// An id the source does not state: no definition, and the model's route
// at this span stands.
export const dynamicTask = task({
  id: dynamicId,
  run: async () => {},
});
