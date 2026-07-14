// wasmCSG.js — async loader + thin wrapper for the Rust/WASM CSG module.
// The WASM binary lives alongside the JS glue in ./wasm/.

let wasmModule = null;
let initPromise = null;

export function initCSGWasm() {
    if (!initPromise) {
        initPromise = import('./wasm/csg_wasm.js').then(async (mod) => {
            await mod.default();   // fetches + instantiates csg_wasm_bg.wasm
            wasmModule = mod;
            console.log('WASM CSG module loaded');
        });
    }
    return initPromise;
}

export function evaluateRegionWasm(regionJSON, worldScale) {
    return wasmModule.evaluate_region(regionJSON, worldScale);
}
