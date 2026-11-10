import { Socket, Other, Wired } from "wire-pkg";

type Wire = Socket<{ a: () => void }>;
class LocalSocket extends Socket {}

declare const aliased: Wire;
declare const both: Socket & Other;
declare const plain: Socket;
declare const maybe: Socket | undefined;
declare const local: LocalSocket;
declare const iface: Wired;
declare const holder: { inner: Socket };

aliased.on("a", () => {});
both.on("b", () => {});
plain.on("c", () => {});
plain.emit("d");
maybe?.on("e", () => {});
local.on("f", () => {});
holder.inner.on("g", () => {});
iface.on("h", () => {});
