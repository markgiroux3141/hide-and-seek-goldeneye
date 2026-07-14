// Left sidebar: collapsible parameter panel. Preserves input ids so
// existing wiring in hud.js and main.js continues to work.

const STORAGE_KEY = 'ge-sidebar-state';

function loadState() {
    try {
        const raw = localStorage.getItem(STORAGE_KEY);
        return raw ? JSON.parse(raw) : { collapsed: false, sections: {} };
    } catch {
        return { collapsed: false, sections: {} };
    }
}

function saveState(s) {
    try { localStorage.setItem(STORAGE_KEY, JSON.stringify(s)); } catch {}
}

export function initSidebar() {
    const sidebar = document.getElementById('sidebar');
    const toggle = document.getElementById('sidebar-toggle');
    if (!sidebar || !toggle) return;

    const state = loadState();

    if (state.collapsed) sidebar.classList.add('collapsed');
    toggle.textContent = sidebar.classList.contains('collapsed') ? '>>' : '<<';

    toggle.addEventListener('click', () => {
        sidebar.classList.toggle('collapsed');
        const collapsed = sidebar.classList.contains('collapsed');
        toggle.textContent = collapsed ? '>>' : '<<';
        state.collapsed = collapsed;
        saveState(state);
    });

    sidebar.querySelectorAll('.sidebar-section').forEach(section => {
        const key = section.dataset.section;
        if (state.sections[key]) section.classList.add('collapsed');

        const header = section.querySelector('.sidebar-section-header');
        if (header) {
            header.addEventListener('click', () => {
                section.classList.toggle('collapsed');
                state.sections[key] = section.classList.contains('collapsed');
                saveState(state);
            });
        }
    });

    // Prevent editor hotkeys (WASD etc.) from firing while typing in a field.
    sidebar.addEventListener('keydown', e => e.stopPropagation());
    sidebar.addEventListener('keyup', e => e.stopPropagation());
}
