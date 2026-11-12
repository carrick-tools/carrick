/**
 * carrick#3146: every program a loader project builds has the same roots, and
 * a rebuild after an edit that changes no import reuses the old program.
 *
 * ts-morph hands the compiler every file it has loaded as a root, so a
 * project's roots grew with each rebuild and a call's program depended on how
 * many rebuilds its process had done before it. The roots are now the first
 * build's, plus every file added through the project API.
 */
export {};
