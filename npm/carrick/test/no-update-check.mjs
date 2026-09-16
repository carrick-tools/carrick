// Every test in this package runs with the update check off.
//
// The check reaches the npm registry and writes a cache under the user's config
// directory, and a good third of these tests spawn `bin/carrick.mjs` as a real
// child process. Without this, running the suite on a laptop dials npm and
// leaves state in ~/.config/carrick — and a test that asserts a command's exact
// output would be asserting against a line that only appears when a release
// happens to be newer than the checkout.
//
// Set as an environment variable rather than a module-level flag, because the
// processes that need it are children: `node --test` spawns one per file, and
// those spawn the shim, and each inherits this.
//
// Loaded by the `test` script's `--import`. Anything already set wins, so a
// test that wants the check on can still ask for it.
process.env["CARRICK_NO_UPDATE_CHECK"] ??= "1";
