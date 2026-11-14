import { getThing, listThings, saveThing, type Thing } from "@fixture/api";

export async function show(draft: boolean) {
  return getThing(draft ? "drafts/latest" : "published/latest");
}

export async function index(archived: boolean) {
  return listThings(archived ? "old/archived" : "new/active");
}

export async function store(thing: Thing) {
  return saveThing(thing);
}
