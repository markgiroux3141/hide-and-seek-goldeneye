// Centralized constants — magic numbers extracted from across the codebase

// Core units
export const WALL_THICKNESS = 1;    // 1 WT = the fundamental unit
export const WORLD_SCALE = 0.25;    // 1 WT = 0.25 Three.js meters

// CSG texturing
export const WALL_SPLIT_V = 6;      // WT height where lower wall meets upper wall
export const CSG_CENTROID_TOL = 0.5; // tolerance (WT) for face identity recovery

// Editor defaults
export const DEFAULT_PLATFORM_SIZE_X = 4;
export const DEFAULT_PLATFORM_SIZE_Z = 4;
export const DEFAULT_PLATFORM_THICKNESS = 1;
export const DEFAULT_STAIR_WIDTH = 4;
export const DEFAULT_STAIR_STEP_HEIGHT = 1;
export const DEFAULT_STAIR_RISE_OVER_RUN = 1;
export const DEFAULT_BRACE_WIDTH = 1;   // WT — size along the wall (U axis)
export const DEFAULT_BRACE_DEPTH = 1;   // WT — protrusion into the room
export const BRACE_ZONE = 7;            // material zone index for brace faces
export const MIN_BRACE_DIM = 1;
export const MAX_BRACE_DIM = 8;
export const DEFAULT_PILLAR_SIZE = 1;   // WT — square cross-section side length
export const MIN_PILLAR_SIZE = 1;
export const MAX_PILLAR_SIZE = 8;

// Brush defaults
export const DEFAULT_BRUSH_RADIUS = 8;
export const DEFAULT_BRUSH_STRENGTH = 0.5;
export const DEFAULT_BRUSH_NOISE_SCALE = 0.1;
export const DEFAULT_BRUSH_NOISE_AMP = 2;
export const MIN_BRUSH_RADIUS = 1;
export const MAX_BRUSH_RADIUS = 50;
export const MIN_BRUSH_STRENGTH = 0.1;
export const MAX_BRUSH_STRENGTH = 1.0;
export const MIN_SUBDIVISION = 1;
export const MAX_SUBDIVISION = 20;

// Light defaults
export const DEFAULT_LIGHT_INTENSITY = 5.0;
export const DEFAULT_LIGHT_RANGE = 20;
export const DEFAULT_LIGHT_Y_OFFSET = 4;
export const DEFAULT_AMBIENT_INTENSITY = 1.0;

// Gizmo dimensions
export const GIZMO_ARROW_LENGTH = 3;
export const GIZMO_SHAFT_RADIUS = 0.12;
export const GIZMO_TIP_LENGTH = 0.7;
export const GIZMO_TIP_RADIUS = 0.3;
export const GIZMO_HANDLE_SIZE = 0.4;
export const GIZMO_DRAG_SENSITIVITY = 0.008;

// Collision
export const MIN_DIMENSION = 1;

// Scene
export const FOG_NEAR = 30;
export const FOG_FAR = 80;
export const INDOOR_BG_COLOR = 0x1a1a2e;
export const TERRAIN_BG_COLOR = 0x111118;
export const TERRAIN_PERSPECTIVE_BG = 0x556677;

// Undo
export const MAX_UNDO = 50;

// Preview offsets
export const PREVIEW_OFFSET_DOOR = 0.01;
export const PREVIEW_OFFSET_EXTRUDE = 0.02;
