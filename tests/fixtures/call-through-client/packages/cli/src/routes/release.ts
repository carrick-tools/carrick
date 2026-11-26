const path = "/api/v1/jobs/release";
const method = "POST";

/** The release route, read by the queue client and by the route table. */
export const route = { path, method };

/** The route table's entry for it. */
export const describe = { ...route, summary: "release a claimed job" };
