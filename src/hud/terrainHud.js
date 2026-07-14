// Terrain mode HUD display

import { state } from '../state.js';
import { getActiveTerrain } from '../tools/ToolManager.js';

const TERRAIN_TOOL_LABELS = { boundary: 'BOUNDARY', hole: 'HOLE', edit: 'EDIT', sculpt: 'SCULPT' };
const BRUSH_LABELS = { raise: 'RAISE/LOWER', noise: 'NOISE', smooth: 'SMOOTH', flatten: 'FLATTEN' };

export function updateTerrainHUD() {
    const statusEl = document.getElementById('status');
    const toolInfoEl = document.getElementById('tool-info');
    const terrainSettingsEl = document.getElementById('terrain-settings');
    if (terrainSettingsEl) terrainSettingsEl.style.display = 'block';
    const lines = [];
    const terrain = getActiveTerrain();

    lines.push(`<span style="color:#ff0">TERRAIN MODE</span>`);

    if (terrain) {
        if (state.terrainTool === 'boundary') {
            if (state.terrainDrawingPhase === 'drawing') {
                lines.push(`Drawing boundary: ${state.terrainDrawingVertices.length} vertices`);
                lines.push(`Click near first vertex to close`);
                lines.push(`Backspace=undo vertex  Esc=cancel`);
            } else if (terrain.isClosed && !terrain.hasMesh) {
                lines.push(`Boundary closed: ${terrain.boundary.length} vertices`);
                lines.push(`G=generate mesh  +/-=subdivision (${terrain.subdivisionLevel})`);
            } else if (terrain.hasMesh) {
                lines.push(`Mesh: ${terrain.vertices.length} verts, ${terrain.triangles.length} tris`);
                lines.push(`G=regenerate mesh  +/-=subdivision (${terrain.subdivisionLevel})`);
            } else {
                lines.push(`Click to place boundary vertices`);
            }
        } else if (state.terrainTool === 'hole') {
            if (state.terrainDrawingPhase === 'drawing') {
                lines.push(`Drawing hole: ${state.terrainDrawingVertices.length} vertices`);
                lines.push(`Click near first vertex to close`);
            } else {
                lines.push(`Click to draw hole polygon`);
                lines.push(`Holes: ${terrain.holes.length}`);
            }
        } else if (state.terrainTool === 'edit') {
            lines.push(`Click to move boundary vertices`);
            lines.push(`Boundary: ${terrain.boundary.length} vertices`);
        } else if (state.terrainTool === 'sculpt') {
            lines.push(`Brush: ${BRUSH_LABELS[state.brushType]}`);
            lines.push(`Radius: ${state.brushRadius} | Strength: ${state.brushStrength.toFixed(1)}`);
            lines.push(`Click+drag=apply  Shift=invert`);
            lines.push(`B=cycle brush  +/-=radius  [/]=strength`);
        }

        lines.push(`Wall: ${terrain.wallStyle} | H=${terrain.wallHeight}`);
    }

    lines.push(`Terrains: ${state.terrainMaps.length}`);
    statusEl.innerHTML = lines.join('<br>');

    const toolName = TERRAIN_TOOL_LABELS[state.terrainTool] || state.terrainTool;
    const camMode = state.terrainCameraMode === 'ortho' ? 'TOP-DOWN' : 'PERSPECTIVE';
    toolInfoEl.innerHTML = `Terrain: ${toolName}<br>Camera: ${camMode}<br>M=indoor  T=tool  Tab=camera  Shift+W=wall`;
}
