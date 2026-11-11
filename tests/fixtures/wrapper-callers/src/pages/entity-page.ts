import { api, inlineApi } from "../clients/api-module";

export async function loadEntities() {
  await api.getEntities();
  await inlineApi.getEntities();
}

export async function submitEntity() {
  await api.create("{}");
  await inlineApi.create("{}");
}
