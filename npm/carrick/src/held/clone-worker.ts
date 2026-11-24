// Runs one `cloneShadow` off the held daemon's event loop (T5, #5038): its tree walks, hashes and copies take
// seconds on a large service, and a daemon blocked that long answers no hello, so asks arriving meanwhile were lost.

import { parentPort, workerData } from "node:worker_threads";
import { cloneShadow, type CloneOptions } from "./shadow.ts";

const job = workerData as { mainOut: string; worktreeTop: string; scanRel: string; out: string; options: Omit<CloneOptions, "copyTree"> };
parentPort?.postMessage(cloneShadow(job.mainOut, job.worktreeTop, job.scanRel, job.out, job.options));
