const path = "/api/v1/jobs/claim";
const method = "POST";

/** The claim route, read by the queue client and by the route table. */
export const route = { path, method };

/** The route table's entry for it. */
export const describe = { ...route, summary: "claim the next jobs" };
