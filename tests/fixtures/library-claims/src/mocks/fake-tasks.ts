import { task } from "@fixture/tasks";

export const fakeTask = task({
  id: "mock-only-task",
  run: async () => {},
});
