// Menu tree definition — data-driven structure for the radial menu

import { TEXTURE_SCHEMES } from '../scene/textureSchemes.js';
import { PLATFORM_STYLES } from '../geometry/platformStyles.js';
import { hotkeyManager } from '../input/HotkeyManager.js';

export function buildMenuTree() {
    return [
        {
            label: 'Tools',
            children: [
                { label: 'CSG',           action: 'tool:csg',           hotkey: hotkeyManager.getDisplayKey('tool_csg'),           hotkeyAction: 'tool_csg' },
                { label: 'Hole Cutter',   action: 'tool:hole',          hotkey: hotkeyManager.getDisplayKey('tool_hole'),          hotkeyAction: 'tool_hole' },
                { label: 'Door Cutter',   action: 'tool:door',          hotkey: hotkeyManager.getDisplayKey('tool_door'),          hotkeyAction: 'tool_door' },
                { label: 'Platform',      action: 'tool:platform',      hotkey: hotkeyManager.getDisplayKey('tool_platform'),      hotkeyAction: 'tool_platform' },
                { label: 'Simple Stairs', action: 'tool:simple_stairs', hotkey: hotkeyManager.getDisplayKey('tool_simple_stairs'), hotkeyAction: 'tool_simple_stairs' },
                { label: 'Light',         action: 'tool:light',         hotkey: hotkeyManager.getDisplayKey('tool_light'),         hotkeyAction: 'tool_light' },
                { label: 'Face Paint',    action: 'tool:face_paint',    hotkey: hotkeyManager.getDisplayKey('tool_face_paint'),    hotkeyAction: 'tool_face_paint' },
                { label: 'Start Cave',    action: 'brush:start_cave_from_face', hotkey: hotkeyManager.getDisplayKey('start_cave_from_face'), hotkeyAction: 'start_cave_from_face' },
            ],
        },
        {
            label: 'Platform Style',
            children: Object.entries(PLATFORM_STYLES).map(([name, def]) => ({
                label: def.label,
                action: `platform_style:${name}`,
            })),
        },
        {
            label: 'Textures',
            children: buildTextureSubmenu(),
        },
        {
            label: 'View',
            children: [
                { label: 'Grid Mode', action: 'view:grid', hotkey: hotkeyManager.getDisplayKey('toggle_view'), hotkeyAction: 'toggle_view' },
                { label: 'Textured Mode', action: 'view:textured', hotkey: hotkeyManager.getDisplayKey('toggle_view'), hotkeyAction: 'toggle_view' },
                { label: 'Toggle Grid Lines', action: 'view:toggle_grid', hotkey: hotkeyManager.getDisplayKey('toggle_grid'), hotkeyAction: 'toggle_grid' },
            ],
        },
        {
            label: 'File',
            children: [
                { label: 'Export GLB', action: 'file:export_glb', hotkey: hotkeyManager.getDisplayKey('export_glb'), hotkeyAction: 'export_glb' },
                { label: 'Bake Level',  action: 'file:bake_glb',   hotkey: hotkeyManager.getDisplayKey('bake_glb'),   hotkeyAction: 'bake_glb' },
            ],
        },
        {
            label: 'Settings',
            children: [
                { label: 'Rebind Keys', action: 'open:rebind' },
                { label: 'Reset All Keys', action: 'reset:hotkeys' },
            ],
        },
    ];
}

function buildTextureSubmenu() {
    // Group schemes by their "group" field (or derive from name prefix).
    // Schemes without a `key` are internal (e.g. used by platform styles)
    // and don't appear in the CSG room-texture menu.
    const groups = {};
    for (const [name, scheme] of Object.entries(TEXTURE_SCHEMES)) {
        if (!scheme.key) continue;
        const group = scheme.group || name.split('_')[0];
        const groupLabel = group.charAt(0).toUpperCase() + group.slice(1);
        if (!groups[groupLabel]) groups[groupLabel] = [];
        groups[groupLabel].push({
            label: scheme.label,
            action: `texture:${name}`,
            hotkey: scheme.key,
        });
    }

    // Always show level grouping (e.g. Facility) even if there's only one
    const groupNames = Object.keys(groups);
    return groupNames.map(g => ({
        label: g,
        children: groups[g],
    }));
}
