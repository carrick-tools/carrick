A service (`apps/portal/ui/web`) with no tsconfig of its own. The only
tsconfig sits two folders up, in `apps/portal`, and its `paths` alias is the
only way the caller's import resolves. A compiler program set up at the
service root alone, without the scan root above it, cannot find that
tsconfig, so the import does not resolve and the caller has no type.

`readGadget` returns a type with one member the source declares `unknown`:
its caller is typed, and the reason beside the type is the capture's own word
for that member.
