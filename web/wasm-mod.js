import { bootstrap } from "./rustradio-ui-bootstrap.js";
await bootstrap({
  pkgName: "ruwasm",
  wasmMemoryConfig: {
    initial: 31,
    maximum: 16384,
    shared: true,
  },
  workerThreadStackSize: 1024 * 1024,
});
