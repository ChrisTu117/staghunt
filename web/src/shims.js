// Browser stand-in for the few Node globals some dependencies read.
export const process = { env: {}, browser: true, version: "", versions: {}, nextTick: (fn, ...a) => queueMicrotask(() => fn(...a)) };
