import { RunClient } from "../client/index.js";

/**
 * The client a consumer ASKS for rather than imports. A caller that goes
 * through this states where its receiver came from, and nothing else.
 */
export const runClientManager = {
  clientOrThrow(): RunClient {
    return new RunClient("https://runs.example.test");
  },
};
