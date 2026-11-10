import {
  Gateway,
  envInline,
  envLocal,
  getterInline,
  getterLocal,
  literalLocal,
  noParameterLocal,
  templateInline,
  templateLocal,
} from "./clients/local-base";

const gateway = new Gateway();

export async function act() {
  await gateway.sendLocal("/items.list");
  await gateway.sendInline("/items.list");
  await gateway.fieldLocal("/items.list");
  await gateway.fieldInline("/items.list");
  await getterLocal("/items.list");
  await getterInline("/items.list");
  await envLocal("/items.list");
  await envInline("/items.list");
  await templateLocal("/items.list");
  await templateInline("/items.list");
  await literalLocal("/items.list");
  await noParameterLocal();
}
