# Blob reader floor

`FLOOR` names the oldest release that must still read every index blob this
build writes. `tests/previous_release_reads_blob_test.rs` runs that release's
binary over a blob from each service of a three-service fixture, and CI's
`steps` shard downloads it from the release's `carrick-action-linux.tar.gz`.

A field leaves the blob in two steps:

1. Stop reading it and keep writing a neutral value (0, `[]`), so older
   siblings still read the blob.
2. Raise `FLOOR` to a release that no longer requires the field, once no
   runner or customer is left below it, and delete the field in that PR.

Set at 0.3.153 after carrick#3762 stopped writing `unemitted_literal_candidates`,
which 0.3.153 requires.
