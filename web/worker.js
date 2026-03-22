import init, { git_version, compute } from "./ruwasm.js";

let wasm_ready = init();

onmessage = async (e) => {
    console.log("Worker got message");
    const { type, data } = e.data;
    await wasm_ready;

    if (type === "init") {
        postMessage({ type: "ready" });
    } else if (type === "git_version") {
        postMessage({ type: "git_version", data: git_version() });
    } else if (type === "compute") {
        const result = compute(data);
        postMessage({ type: "result", data: result });
    }
};
