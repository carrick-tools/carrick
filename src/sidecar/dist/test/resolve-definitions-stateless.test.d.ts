/**
 * `resolve_definitions` reads a capture stub and nothing else (carrick#1927).
 *
 * The resolver builds a project of its own over the stub's `types/` tree. The
 * service's project, the one `init` names and `infer` reads, has no part in
 * the answer, so the request must not build it. It used to: the handler took
 * its resolver from the project's components, and the first request to do so
 * in a process builds the whole program. The scanner asks each capture of a
 * fresh process, so the `resolve_definitions` after it was always that first
 * request, and built a program the size of the service to read a tree the
 * size of its surface.
 *
 * Three processes answer the same request over the same stub:
 *  - one that was never `init`'d,
 *  - one that was `init`'d and asked nothing else,
 *  - one that had built the service's project before it was asked.
 * All three give the same definitions, and only the third ever builds.
 *
 * The log is read after the process has closed, so a line it wrote cannot be
 * missed and a line it did not write cannot arrive later.
 */
export {};
