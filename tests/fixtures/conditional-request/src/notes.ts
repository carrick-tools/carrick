declare function isNew(): boolean;

export async function save(existing: { id: string } | null, input: object) {
  const method = existing ? "PATCH" : "POST";
  const url = existing ? `/api/notes/${existing.id}` : "/api/notes";
  await fetch(url, { method, body: JSON.stringify(input) });
}

export async function saveInline(existing: { id: string } | null, input: object) {
  await fetch(existing ? `/api/notes/${existing.id}` : "/api/notes", {
    method: existing ? "PATCH" : "POST",
    body: JSON.stringify(input),
  });
}

export async function reorder(first: boolean) {
  await fetch("/api/notes/reorder", { method: first ? "PUT" : "POST" });
}

export async function saveTwoTests(editing: boolean, hasId: boolean, id: string) {
  const method = editing ? "PATCH" : "POST";
  const url = hasId ? `/api/notes/${id}` : "/api/notes";
  await fetch(url, { method });
}

export async function saveCalledTest(id: string) {
  const method = isNew() ? "POST" : "PATCH";
  const url = isNew() ? "/api/notes" : `/api/notes/${id}`;
  await fetch(url, { method });
}

export async function saveReassigned(isEdit: boolean, id: string) {
  const method = isEdit ? "PATCH" : "POST";
  isEdit = !isEdit;
  const url = isEdit ? `/api/notes/${id}` : "/api/notes";
  await fetch(url, { method });
}

export async function saveUnreadMethod(existing: boolean, id: string, verb: string) {
  const url = existing ? `/api/notes/${id}` : "/api/notes";
  await fetch(url, { method: verb });
}

export async function saveNegated(existing: { id: string } | null, input: object) {
  const method = !existing ? "POST" : "PATCH";
  const url = existing ? `/api/notes/${existing.id}` : "/api/notes";
  await fetch(url, { method, body: JSON.stringify(input) });
}

export async function saveByMode(mode: string, id: string) {
  const method = mode === "edit" ? "PATCH" : "POST";
  const url = mode === "copy" ? "/api/notes" : `/api/notes/${id}`;
  await fetch(url, { method });
}

export async function saveScoped(shared: boolean, mode: string, id: string) {
  const method = shared ? (mode === "replace" ? "PUT" : "PATCH") : "POST";
  const url = `/api/${shared ? "shared" : "own"}/notes/${id}/${mode === "append" ? "lines" : "body"}`;
  await fetch(url, { method });
}

export async function saveByChain(mode: string, id: string) {
  const method = mode === "edit" ? "PATCH" : mode === "copy" ? "PUT" : "POST";
  const url = mode === "edit" ? `/api/notes/${id}` : mode === "copy" ? `/api/notes/${id}/copy` : "/api/notes";
  await fetch(url, { method });
}

export async function saveByChainSplit(mode: string, id: string) {
  const method = mode === "copy" ? "PUT" : "POST";
  const url = mode === "edit" ? `/api/notes/${id}` : mode === "copy" ? `/api/notes/${id}/copy` : "/api/notes";
  await fetch(url, { method });
}
