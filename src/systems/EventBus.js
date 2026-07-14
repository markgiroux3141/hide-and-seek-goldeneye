// Typed synchronous pub/sub event bus
// Decouples producers (commands, actions) from consumers (renderers, HUD)

const listeners = new Map();

export function on(event, callback) {
    if (!listeners.has(event)) listeners.set(event, new Set());
    listeners.get(event).add(callback);
    return () => listeners.get(event).delete(callback);
}

export function off(event, callback) {
    const set = listeners.get(event);
    if (set) set.delete(callback);
}

export function emit(event, data) {
    const set = listeners.get(event);
    if (set) {
        for (const cb of set) cb(data);
    }
}
