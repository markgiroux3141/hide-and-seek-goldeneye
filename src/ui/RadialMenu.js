// Radial "clock" menu — middle-click to open, hierarchical submenus

import { emit } from '../systems/EventBus.js';
import { hotkeyManager, eventToBinding } from '../input/HotkeyManager.js';

export class RadialMenu {
    constructor() {
        this.root = document.getElementById('radial-menu');
        this.centerEl = null;
        this.itemEls = [];
        this.menuStack = [];   // for back navigation
        this.currentItems = [];
        this.currentTitle = null;
        this.onClose = null;
        this._rebindMode = false;       // when true, clicking leaf items rebinds instead of executing
        this._captureActionId = null;   // when set, next keypress rebinds this action
        this._buildTree = null;         // function to rebuild menu tree (for rebind mode)

        // Close on backdrop click
        this.root.addEventListener('mousedown', (e) => {
            if (e.target === this.root && !this._captureActionId) this.close();
        });

        // Close on Escape / capture rebind key
        this._onKeyDown = (e) => {
            if (!this.isOpen()) return;

            // Capture mode: next keypress rebinds the selected action
            if (this._captureActionId) {
                e.preventDefault();
                e.stopPropagation();
                if (e.code === 'Escape') {
                    // Cancel capture, stay in rebind mode
                    this._captureActionId = null;
                    this._render(this.currentItems, this.currentTitle);
                    return;
                }
                const binding = eventToBinding(e);
                if (binding) {
                    hotkeyManager.rebind(this._captureActionId, binding);
                    this._captureActionId = null;
                    this._render(this.currentItems, this.currentTitle);
                }
                return;
            }

            if (e.code === 'Escape') {
                e.preventDefault();
                e.stopPropagation();
                if (this._rebindMode) {
                    // Exit rebind mode, re-render current view normally
                    this._rebindMode = false;
                    this._render(this.currentItems, this.currentTitle);
                } else {
                    this.close();
                }
            }
        };
        document.addEventListener('keydown', this._onKeyDown, true);
    }

    isOpen() {
        return this.root.style.display === 'flex';
    }

    open(items, onClose, buildTree) {
        this.onClose = onClose;
        this._buildTree = buildTree || null;
        this.menuStack = [];
        this._rebindMode = false;
        this._captureActionId = null;
        this._render(items, null);
        this.root.style.display = 'flex';
    }

    close() {
        this.root.style.display = 'none';
        this._clear();
        this.menuStack = [];
        this._rebindMode = false;
        this._captureActionId = null;
        if (this.onClose) this.onClose();
    }

    _render(items, title) {
        this._clear();
        this.currentItems = items;
        this.currentTitle = title;

        // Center element
        this.centerEl = document.createElement('div');
        this.centerEl.className = 'radial-center';
        if (this._captureActionId) {
            this.centerEl.textContent = 'PRESS KEY';
            this.centerEl.style.borderColor = '#ff0';
        } else if (this._rebindMode) {
            this.centerEl.textContent = title ? `REBIND: ${title}` : 'REBIND';
            this.centerEl.style.borderColor = '#ff0';
        } else {
            this.centerEl.textContent = title || 'MENU';
        }
        this.root.appendChild(this.centerEl);

        const count = items.length;
        const radius = Math.max(120, count * 25);
        const angleStep = (2 * Math.PI) / count;
        const startAngle = -Math.PI / 2;

        for (let i = 0; i < count; i++) {
            const item = items[i];
            const angle = startAngle + angleStep * i;
            const x = Math.cos(angle) * radius;
            const y = Math.sin(angle) * radius;

            const el = document.createElement('div');
            el.className = 'radial-item';
            el.style.transform = `translate(${x}px, ${y}px)`;

            // Highlight the item being captured
            if (this._captureActionId && this._captureActionId === item.hotkeyAction) {
                el.classList.add('radial-item-active');
            }

            const label = document.createElement('span');
            label.className = 'radial-label';
            label.textContent = item.label;
            el.appendChild(label);

            // Show hotkey — in rebind mode, show current binding from manager
            if (this._rebindMode && item.hotkeyAction) {
                const hk = document.createElement('span');
                hk.className = 'radial-hotkey';
                if (this._captureActionId === item.hotkeyAction) {
                    hk.textContent = '[ ... ]';
                    hk.style.color = '#ff0';
                } else {
                    hk.textContent = hotkeyManager.getDisplayKey(item.hotkeyAction);
                    if (!hotkeyManager.isDefault(item.hotkeyAction)) hk.style.color = '#ff0';
                }
                el.appendChild(hk);
            } else if (item.hotkey) {
                const hk = document.createElement('span');
                hk.className = 'radial-hotkey';
                hk.textContent = item.hotkey;
                el.appendChild(hk);
            }

            if (item.children) {
                label.textContent = item.label + ' \u25B8';
                el.addEventListener('click', (e) => {
                    e.stopPropagation();
                    this.menuStack.push({ items: this.currentItems, title });
                    this._render(item.children, item.label);
                });
            } else if (this._rebindMode && item.hotkeyAction) {
                // In rebind mode: click to start capturing a new key
                el.addEventListener('click', (e) => {
                    e.stopPropagation();
                    this._captureActionId = item.hotkeyAction;
                    this._render(this.currentItems, this.currentTitle);
                });
            } else if (item.action === 'open:rebind') {
                el.addEventListener('click', (e) => {
                    e.stopPropagation();
                    this._rebindMode = true;
                    // Rebuild tree fresh and go to top level in rebind mode
                    this.menuStack = [];
                    const freshTree = this._buildTree ? this._buildTree() : this.currentItems;
                    this._render(freshTree, null);
                });
            } else if (item.action === 'reset:hotkeys') {
                el.addEventListener('click', (e) => {
                    e.stopPropagation();
                    hotkeyManager.resetAll();
                    this._render(this.currentItems, this.currentTitle);
                });
            } else if (item.action) {
                el.addEventListener('click', (e) => {
                    e.stopPropagation();
                    emit('menu:action', { actionId: item.action });
                    this.close();
                });
            }

            this.root.appendChild(el);
            this.itemEls.push(el);
        }

        // Back button if in a submenu
        if (this.menuStack.length > 0) {
            const backEl = document.createElement('div');
            backEl.className = 'radial-item radial-back';
            backEl.textContent = '\u25C2 Back';
            backEl.style.transform = `translate(0px, ${radius + 50}px)`;
            backEl.addEventListener('click', (e) => {
                e.stopPropagation();
                this._captureActionId = null;
                const prev = this.menuStack.pop();
                this._render(prev.items, prev.title);
            });
            this.root.appendChild(backEl);
            this.itemEls.push(backEl);
        }
    }

    _clear() {
        while (this.root.firstChild) {
            this.root.removeChild(this.root.firstChild);
        }
        this.itemEls = [];
        this.centerEl = null;
    }
}
