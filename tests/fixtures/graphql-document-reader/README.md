# graphql-document-reader

The base tree for the `graphql_document_sites` unit tests (carrick#3504 S9):
a root service with no source files. Each test writes its own files into a
copy (`fixture_walk::walked`'s `edit`) and reads them with the compiler's
rows, which the document reader needs to name any declaration.
