// Procedural textures and materials

import * as THREE from 'three';
import { TEXTURE_SCHEMES } from './textureSchemes.js';

function createCanvasTexture(color1, color2, lineColor) {
    const size = 64;
    const canvas = document.createElement('canvas');
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext('2d');

    ctx.fillStyle = color1;
    ctx.fillRect(0, 0, size, size);

    // Subtle noise
    for (let i = 0; i < 800; i++) {
        ctx.fillStyle = color2;
        ctx.fillRect(Math.random() * size, Math.random() * size, 1, 1);
    }

    // Grid line border
    ctx.strokeStyle = lineColor;
    ctx.lineWidth = 1;
    ctx.strokeRect(0, 0, size, size);

    const tex = new THREE.CanvasTexture(canvas);
    tex.wrapS = THREE.RepeatWrapping;
    tex.wrapT = THREE.RepeatWrapping;
    tex.magFilter = THREE.LinearFilter;
    tex.minFilter = THREE.LinearMipmapLinearFilter;
    return tex;
}

// Shared base textures (cloned per material instance)
let wallTex, floorTex, ceilingTex, highlightTex;

// Loaded BMP textures keyed by name (without path/extension)
const textureMap = new Map();

export function initMaterials() {
    wallTex = createCanvasTexture('#808080', '#777777', '#666666');
    floorTex = createCanvasTexture('#6b6b4e', '#636346', '#5a5a42');
    ceilingTex = createCanvasTexture('#909090', '#888888', '#777777');
    highlightTex = createCanvasTexture('#44aa44', '#55bb55', '#33aa33');
}

// Kick off BMP texture loads. Returns a promise that resolves when all
// textures (including the railing alpha-keyed one) have populated textureMap.
// Deferred from initMaterials() so it doesn't block first paint.
export function loadBmpTextures() {
    const textureNames = new Set();
    for (const scheme of Object.values(TEXTURE_SCHEMES)) {
        for (const zone of Object.values(scheme.zones)) {
            if (zone.texture) textureNames.add(zone.texture);
        }
    }

    const loader = new THREE.TextureLoader();
    const loadOne = (name) => new Promise((resolve) => {
        loader.load(`public/textures/${name}.bmp`, (tex) => {
            tex.wrapS = THREE.RepeatWrapping;
            tex.wrapT = THREE.RepeatWrapping;
            tex.magFilter = THREE.LinearFilter;
            tex.minFilter = THREE.LinearMipmapLinearFilter;
            textureMap.set(name, tex);
            resolve();
        }, undefined, () => resolve());
    });

    const railingPromise = new Promise((resolve) => {
        loader.load('public/transparent_textures/railing.bmp', (tex) => {
            const img = tex.image;
            const canvas = document.createElement('canvas');
            canvas.width = img.width;
            canvas.height = img.height;
            const ctx = canvas.getContext('2d');
            ctx.drawImage(img, 0, 0);
            const imageData = ctx.getImageData(0, 0, canvas.width, canvas.height);
            const data = imageData.data;
            for (let i = 0; i < data.length; i += 4) {
                if (data[i] < 10 && data[i + 1] < 10 && data[i + 2] < 10) {
                    data[i + 3] = 0;
                }
            }
            ctx.putImageData(imageData, 0, 0);
            const rgbaTex = new THREE.CanvasTexture(canvas);
            rgbaTex.wrapS = THREE.RepeatWrapping;
            rgbaTex.wrapT = THREE.RepeatWrapping;
            rgbaTex.magFilter = THREE.LinearFilter;
            rgbaTex.minFilter = THREE.LinearMipmapLinearFilter;
            textureMap.set('railing', rgbaTex);

            // Build a luminance-from-alpha companion for shadow alphaMap.
            // MeshDistanceMaterial reads alphaMap's R channel, not the main map's A,
            // so we splat the alpha channel into RGB on a second canvas.
            const alphaCanvas = document.createElement('canvas');
            alphaCanvas.width = canvas.width;
            alphaCanvas.height = canvas.height;
            const aCtx = alphaCanvas.getContext('2d');
            const aImage = aCtx.createImageData(canvas.width, canvas.height);
            const aData = aImage.data;
            for (let i = 0; i < data.length; i += 4) {
                const a = data[i + 3];
                aData[i] = a; aData[i + 1] = a; aData[i + 2] = a; aData[i + 3] = 255;
            }
            aCtx.putImageData(aImage, 0, 0);
            const alphaTex = new THREE.CanvasTexture(alphaCanvas);
            alphaTex.wrapS = THREE.RepeatWrapping;
            alphaTex.wrapT = THREE.RepeatWrapping;
            alphaTex.magFilter = THREE.LinearFilter;
            alphaTex.minFilter = THREE.LinearMipmapLinearFilter;
            textureMap.set('railing_alpha', alphaTex);
            resolve();
        }, undefined, () => resolve());
    });

    return Promise.all([...[...textureNames].map(loadOne), railingPromise]);
}

export function getWallMaterial() {
    return new THREE.MeshLambertMaterial({ map: wallTex.clone(), side: THREE.FrontSide });
}

export function getFloorMaterial() {
    return new THREE.MeshLambertMaterial({ map: floorTex.clone(), side: THREE.FrontSide });
}

export function getCeilingMaterial() {
    return new THREE.MeshLambertMaterial({ map: ceilingTex.clone(), side: THREE.FrontSide });
}

export function getHighlightMaterial() {
    return new THREE.MeshLambertMaterial({
        map: highlightTex.clone(),
        side: THREE.FrontSide,
        emissive: 0x224422,
        emissiveIntensity: 0.5,
    });
}

export function getDoorFrameMaterial() {
    return new THREE.MeshLambertMaterial({ color: 0x8B7355, side: THREE.FrontSide });
}

export function getDoorExitMaterial() {
    return new THREE.MeshLambertMaterial({ color: 0x556655, side: THREE.FrontSide });
}

// Build material array for a specific texture scheme.
// Returns array of 8 materials indexed by zone (0-7).
// `side` defaults to FrontSide; pass DoubleSide for plane-based styles whose
// back faces should also be visible (e.g. simple-style platforms/stairs).
export function getTexturedMaterialArrayForScheme(schemeName, side = THREE.FrontSide) {
    const scheme = TEXTURE_SCHEMES[schemeName] || TEXTURE_SCHEMES.facility_white_tile;

    return Object.keys(scheme.zones).sort((a, b) => a - b).map(zoneIdx => {
        const zone = scheme.zones[zoneIdx];
        if (zone.texture === null) {
            return new THREE.MeshLambertMaterial({
                color: zone.color,
                side,
                vertexColors: true,
            });
        }
        const baseTex = textureMap.get(zone.texture);
        if (!baseTex) {
            return new THREE.MeshLambertMaterial({
                color: 0xff00ff, // magenta = missing texture
                side,
                vertexColors: true,
            });
        }
        const t = baseTex.clone();
        t.repeat.set(zone.repeat, zone.repeat);
        if (zone.offsetX || zone.offsetY) t.offset.set(zone.offsetX || 0, zone.offsetY || 0);
        if (zone.rotation) t.rotation = zone.rotation * (Math.PI / 180); // degrees to radians
        t.needsUpdate = true;
        return new THREE.MeshLambertMaterial({
            map: t,
            side,
            vertexColors: true,
        });
    });
}

// Convenience alias for default scheme
export function getTexturedMaterialArray() {
    return getTexturedMaterialArrayForScheme('facility_white_tile');
}

// ─── CSG materials bridge ────────────────────────────────────────────
// CSG meshes call getCSGMaterialsForScheme(name) once per unique scheme
// per rebuild. We cache the result so successive rebuilds reuse the same
// THREE.Material instances (which are safe to share across meshes).
const csgSchemeCache = new Map();

export function getCSGMaterialsForScheme(schemeName) {
    if (!csgSchemeCache.has(schemeName)) {
        csgSchemeCache.set(schemeName, getTexturedMaterialArrayForScheme(schemeName));
    }
    return csgSchemeCache.get(schemeName);
}

// Clear the CSG material cache. Call this if texture schemes are reloaded
// (e.g., during dev) to force fresh materials on the next CSG rebuild.
export function clearCSGMaterialCache() {
    csgSchemeCache.clear();
}

// Double-sided alpha-tested material for railings.
// The railing BMP is converted to RGBA during init (black → transparent).
// Includes a customDistanceMaterial so point-light shadows respect the alpha cutout.
export function getRailingMaterial() {
    const baseTex = textureMap.get('railing');
    if (!baseTex) {
        return new THREE.MeshLambertMaterial({ color: 0xff00ff, side: THREE.DoubleSide });
    }
    const t = baseTex.clone();
    t.repeat.set(1.0, 1.0);
    t.needsUpdate = true;
    const mat = new THREE.MeshLambertMaterial({
        map: t,
        side: THREE.DoubleSide,
        alphaTest: 0.5,
        transparent: true,
    });

    const alphaSrc = textureMap.get('railing_alpha');
    if (alphaSrc) {
        const distAlpha = alphaSrc.clone();
        distAlpha.repeat.copy(t.repeat);
        distAlpha.needsUpdate = true;
        mat.customDistanceMaterial = new THREE.MeshDistanceMaterial({
            alphaMap: distAlpha,
            alphaTest: 0.5,
            depthPacking: THREE.RGBADepthPacking,
        });
    }
    return mat;
}

// Simple wireframe-style material for railings in grid mode
export function getRailingGridMaterial() {
    return new THREE.MeshLambertMaterial({
        color: 0xaaaa55,
        side: THREE.DoubleSide,
        opacity: 0.5,
        transparent: true,
    });
}
