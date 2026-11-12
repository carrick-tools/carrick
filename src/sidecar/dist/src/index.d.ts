/**
 * Main entry point for the type-sidecar
 *
 * This module implements a message loop that:
 * 1. Listens on stdin for JSON requests
 * 2. Processes each request (see handleRequest for the actions)
 * 3. Writes JSON responses to stdout
 *
 * IMPORTANT:
 * - stdout is ONLY for JSON responses
 * - stderr is for logging
 * - Process stays alive between requests (warm standby)
 * - One request at a time. A handler that runs the compiler blocks the event
 *   loop until it returns, so a request sent meanwhile waits in the pipe, and
 *   nothing can cancel the one that is running. The scanner kills a process
 *   that goes silent past its deadline; a long handler stays alive by writing
 *   `progress` frames as it finishes units of work (writeProgress).
 */
export {};
