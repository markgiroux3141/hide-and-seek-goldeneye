// localStorage auto-save and restore

const STORAGE_KEY = 'goldeneye-level';

export function saveToLocalStorage(json) {
    localStorage.setItem(STORAGE_KEY, json);
}

export function loadFromLocalStorage() {
    return localStorage.getItem(STORAGE_KEY);
}
