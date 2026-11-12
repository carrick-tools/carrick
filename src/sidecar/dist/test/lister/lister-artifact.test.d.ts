/**
 * carrick#1660: the surface lister as a released artifact. `carrick-lister.mjs`
 * is the built sidecar in one file, run by the cloud's library store on its
 * Node 22 runtime with a bare environment, against a directory holding one
 * package and its installed type closure.
 *
 * By default the test builds the artifact itself, which needs the build's
 * own install (`npm ci --prefix lister`); it sits outside `dist/test/*.test.js`
 * so the sidecar suite does not need it. The release workflow sets
 * CARRICK_LISTER_ARTIFACT to the entry it is about to attach, so the bytes
 * tested are the bytes released.
 */
export {};
