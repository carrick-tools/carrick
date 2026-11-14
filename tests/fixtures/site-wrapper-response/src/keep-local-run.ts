import { runJob } from "./keep-local-run-lib";
export const job = () => runJob<{ done: boolean }>("/jobs");
