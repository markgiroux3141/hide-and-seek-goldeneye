// wasmCave.js — async loader + thin wrapper for the Rust/WASM cave module.
// The WASM binary lives alongside the JS glue in ./wasm/.
//
// Rebuild from the source crate with:
//   cd spike/cave-painting-rust/cave-wasm && wasm-pack build --target web --out-dir pkg --release
// then copy pkg/cave_wasm.js + pkg/cave_wasm_bg.wasm into ./wasm/.

let wasmModule = null;
let initPromise = null;

export function initCaveWasm() {
    if (!initPromise) {
        initPromise = import('./wasm/cave_wasm.js').then(async (mod) => {
            await mod.default();   // fetches + instantiates cave_wasm_bg.wasm
            wasmModule = mod;
            console.log('WASM Cave module loaded');
        });
    }
    return initPromise;
}

export function createCaveWorld(voxelSize, defaultDensity) {
    if (!wasmModule) {
        throw new Error('createCaveWorld called before initCaveWasm() resolved');
    }
    return new wasmModule.CaveWorld(voxelSize, defaultDensity);
}
