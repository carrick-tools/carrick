const { formatTotal: ft } = require("../common/money") as typeof import("./decoy");

export const viaLocal = (c: number) => ft(c);
