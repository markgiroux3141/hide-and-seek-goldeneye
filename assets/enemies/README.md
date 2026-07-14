# Enemy Assets

Engine-neutral character models and animations for the HUNT-phase enemies.
These are **durable artifacts** — they outlive the Three.js prototype and feed
the eventual C++ engine directly (glTF 2.0 is read natively by `cgltf`,
`tinygltf`, and `assimp`).

## Folder layout

```
source/     Original Mixamo .fbx — source of truth. Re-derive glb/ from these.
glb/        Converted .glb (mesh + embedded animation clips) — what engines load.
textures/   Standalone texture maps if not embedded in the glb.
```

## Where these come from — Mixamo pipeline

Mixamo (free, royalty-free, needs an Adobe account) only exports FBX. Web wants
glТF, and so does our future C++ loader, so we convert once and both engines consume it.

1. **Character** — pick a model, download **FBX, T-pose, no animation**.
2. **Animations** — download each clip as **FBX, "Without Skin"** (~50–200 KB each).
   Every Mixamo rig shares ONE skeleton ("mixamorig" bones), so any clip plays
   on any character — buy the clip set once, reskin per enemy type.
3. **Convert** — import character + all clips into Blender (or FBX2glTF), export a
   single `.glb` with the mesh and every `AnimationClip` embedded → `glb/`.
4. Commit **both** the FBX source and the derived GLB.

## Engine gotchas (important — see src/game/enemy.js)

- **Scale:** world uses `WORLD_SCALE = 0.25` (1 WT = 0.25 m). Import at real human
  height (~1.8 m) then scale to ~`7.2 * WT`. The current capsule is ~1.5 m / 6 WT.
- **Origin:** Mixamo rigs are **feet-at-origin** — better than the capsule, whose
  origin is its center. Drop the `+3*WT` lift in `_syncMesh()` when using a model;
  place the mesh directly at `this.pos`.
- **Facing:** rotate the model toward movement with `Math.atan2(dx, dz)`.

## Naming convention

```
glb/grunt.glb          character with embedded clips
source/grunt_tpose.fbx
source/grunt@walk.fbx  the @clip suffix is the Mixamo convention
source/grunt@idle.fbx
source/grunt@attack.fbx
```

## Enemy → character → animation mapping (from DESIGN.md §5)

Enemies differ by behavior + silhouette; one clip set, many skins.

| Design enemy      | Mixamo character   | Clips to grab                                     |
|-------------------|--------------------|---------------------------------------------------|
| Grunt             | Swat / Soldier     | Idle, Walking, Running, Looking Around, Death     |
| Breacher          | Mutant / brute     | Walk, Smash/Punch, Roar                           |
| Spotter           | Robot / drone-ish  | Idle, Point, Alert, Scanning                      |
| Sound sensor      | (reuse Robot)      | Idle, Look Around                                 |
| Thermal / Grid    | (reuse Soldier)    | Search, Crouch Walk, Peeking                      |

For the **catch** (touch = caught in `Enemy.update`), fire a Punch / Zombie-Attack
clip on contact. AI states map to: **Idle → Walk/Run → Attack**.

## License

Mixamo output is royalty-free for use in projects (commercial included). You may
not resell the raw Mixamo assets themselves. Keep this note with the assets.
