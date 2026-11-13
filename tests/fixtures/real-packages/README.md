# real-packages: stand-ins for packages a test names by their real name

A scan asks the library store only about packages it finds installed from the
public registry, and only a call the store labels is a candidate (carrick#3711).
A test that writes its source under a real package name (`express`, `hono`)
cannot install the real package offline, so each directory here holds what the
scan reads of it:

- `package.json` and `index.d.ts`: a minimal install, covering only the
  declarations the tests' sources use;
- `claims.json`: the store's answer for that package, stating only the
  operations the tests reach.

`tests/support/real_packages.rs` copies them into a test's repo and writes the
cassette the offline scan reads the answer from. Nothing here is a copy of the
real package.
