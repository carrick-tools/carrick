import { connect, Jobs } from "fixture-jobs";

// (q1) Held.
export async function heldQueue() {
  const jobs = new Jobs();
  const queue = jobs.queue("mail");
  await queue.add("welcome", {});
}

// (q2) The member call on a library function's value.
export async function chainedQueue() {
  await connect("redis://jobs").queue("billing").add("invoice", {});
}

// (q3) The member call on a value an in-repo factory returns.
function jobs(): Jobs {
  return new Jobs();
}

export async function factoryQueue() {
  await jobs().queue("reports").add("daily", {});
}

// (n1) Negative: an in-repo class with members of the same names.
class LocalJobs {
  queue(name: string) {
    return { add: async (job: string, data: unknown) => [name, job, data] };
  }
}

export async function localQueue() {
  await new LocalJobs().queue("local").add("x", {});
}
