let wasm;

onmessage = async (e) => {
    const { type, data } = e.data;

    if (type === "init") {
        const response = await fetch("add.wasm");
        const buffer = await response.arrayBuffer();
        const { instance } = await WebAssembly.instantiate(buffer);
        wasm = instance.exports;
        postMessage({ type: "ready" });
    } else if (type === "compute") {
        const result = wasm.compute(data);
        postMessage({ type: "result", data: result });
    }
};
