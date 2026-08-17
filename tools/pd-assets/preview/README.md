# Perfect Dark import — verification renders

Every claim in [HANDOFF_PD_ASSETS.md](../../../HANDOFF_PD_ASSETS.md) that could be
checked by eye, checked by eye. Open the PNGs directly.

They are all regenerable — nothing here is hand-made:

```sh
# poses the ENGINE computed (real gltf_skin / clip / Skeleton path)
cd native && cargo run --release --example pd_pose_dump -- pd_a51guard 03-run 8 out/

# draw them, or draw straight from a GLB
python tools/pd-assets/pd_preview.py <model.glb> out.png --positions out/pd_a51guard_03-run.f32
python tools/pd-assets/pd_preview.py <model.glb> out.png --clip <clip.glb> --frames 8

# and BEFORE rendering anything: screen a clip's posture curve, which is the one
# thing a contact sheet cannot show (every tile is framed independently on purpose)
python tools/pd-assets/pd_triage.py <clip.glb> [<clip.glb> ...]
```

## Hunters wear them

| Image | What it shows |
|---|---|
| [25-pd-hunter-in-game.png](25-pd-hunter-in-game.png) | **The point of the whole track.** Joanna Dark hunting the player in the `PD_LAB`, firing — on the GPU, in the running game, not in a CPU preview. Textured, skinned, two-handed grip, muzzle flash at the barrel. |
| [26-pd-hunter-close.png](26-pd-hunter-close.png) | Cassandra at arm's length, before the arena was resized. Worth keeping for the face and the brooch at full size. |
| [27-pd-firefight.png](27-pd-firefight.png) | Six DarkSims in the lab. Cassandra firing a rifle, Joanna firing with a muzzle flash — **at each other** — and Mr Blonde face-down and bloodied from an *authored* Perfect Dark death animation rather than a ragdoll. That one frame is three separate things working: PD bodies, PD's per-hit-part death tables, and simulant-versus-simulant fire. |
| [22-death-engine-posed.png](22-death-engine-posed.png) | A PD death (`ANIM_DEATH_0021`) as **the engine itself** poses it, through `gltf_skin` / `clip` / `Skeleton::skinning_matrices` — stands, staggers, buckles, lands face-down. |
| [23-shared-animation-bank.png](23-shared-animation-bank.png) | **Why the hunter clip set exists.** GoldenEye's `21-death-forward-face-down-hard` on Karl above, Perfect Dark's `ANIM_DEATH_0021` on the A51 guard below. Same 118 frames, same fall, different rig — the two games share one animation bank, which is what lets a PD hunter fill the same 36 slots. Measured by `pd_animmap.py`. |
| [24-root-motion-fix.png](24-root-motion-fix.png) | **The bug that only a death could show.** `ANIMFIELD_08` — PD's root-motion channel — sits in each frame *ahead of* the rotation bits. Skipping it read every rotation 28–42 bits early, giving a perfectly coherent body that is permanently lying down (top row). It hid because the only clip anyone had rendered, `ANIM_TWO_GUN_HOLD`, is authored dead still: all four of its bit lengths are 0, so on that one animation the misalignment was exactly zero. |

## The bodies

| Image | What it shows |
|---|---|
| [20-textured-roster-6.png](20-textured-roster-6.png) | The shipped lineup, all six wearing Perfect Dark's own textures — Joanna in the dragon dress, an A51 guard, Elvis the Maian, a dd_shock trooper, Cassandra, Mr Blonde. |
| [01-every-body-every-clip.png](01-every-body-every-clip.png) | The whole shipped set: 6 bodies × 4 locomotion clips, **posed by the engine itself**. Columns idle / walk / jog / run. Fixed camera, so the heights compare — Elvis reads short because Maians are a metre tall. (Regenerated after the root-motion fix; the previous version was drawn from clips whose rotations were 28–29 bits out.) |
| [10-textured-roster.png](10-textured-roster.png) | An earlier four-body lineup, kept because Joanna is on the debug palette there — that is what a body looked like before the pool codec landed. |
| [11-textured-guard-4-angles.png](11-textured-guard-4-angles.png) | The A51 guard from four sides with real PD textures — cap, uniform, chest plate, shoulder patch, boots, belt pouches. |
| [02-walk-cycle.png](02-walk-cycle.png) | 8 frames of `ANIM_0028`, the clip PD itself picks to walk. Weight shifts, arms counter-swing, feet plant. |
| [03-run-cycle.png](03-run-cycle.png) | 6 frames of the sprint. Nothing tears at the joints — the failure this exact view caught when skinning was per mesh node instead of per vertex. |
| [04-scale-vs-goldeneye.png](04-scale-vs-goldeneye.png) | Karl and Pete (GoldenEye) beside two PD bodies, same camera. The PD bodies are genuinely taller: 1.73 m against 1.50 m, because `CHAR_SCALE` shrinks the GE bodies 20%. **Resolved by keeping both**: the game now measures each body's standing height at load and scales its hit capsule and hit-zone boundaries to it (`World::body_capsule`), so a PD hunter's head is inside its own collider instead of 23 cm above a GoldenEye-sized one. |

## How each conversion decision was checked

| Image | What it proves |
|---|---|
| [08-goldeneye-karl-bone9.png](08-goldeneye-karl-bone9.png) + [09-pd-guard-bone9.png](09-pd-guard-bone9.png) | Handedness and facing. `Bone_9` in red sits on the character's **right** on both, and both face **+Z** at yaw 0 — PD checked against a GoldenEye asset already known to work. |
| [05-heads-attached.png](05-heads-attached.png) | Heads close up. The first three have one built in — only 3 of 65 bodies do — while Joanna's is a separate `head*.bin` grafted at the `HEADSPOT`, where a wrong graft point throws it 20 cm off the neck. |
| [06-four-angles.png](06-four-angles.png) | Silhouette coherent from every side, no holes — the symptom of the dropped `G_TRI1` opcode. |
| [07-rest-pose.png](07-rest-pose.png) | The bind pose, which *should* look wrong. PD has no separate bind pose: the node tree is the skeleton and every rotation lives in the animation, so "no clip" is a splayed star. Seeing exactly this confirms the rest offsets and hierarchy before any clip is involved. |

## The texture format, worked out on screen

| Image | What it shows |
|---|---|
| [12-texture-sheet-30.png](12-texture-sheet-30.png) | All 30 of a51guard's inline textures, decoded from RGBA5551. |
| [21-joanna-pool-textures.png](21-joanna-pool-textures.png) | Joanna's 36 textures out of the *compressed* pool — the dragon embroidery, her face, hair, skin. A different codec from the inline ones (`pd_tex.py`). |
| [13-swizzle-before-after.png](13-swizzle-before-after.png) | **The one that mattered.** Top row as-stored, bottom row with odd-row u32 words swapped. The face goes from combed to clean. Found by eye first; `tex_swizzle` (`game/texdecompress.c:1927`) then confirmed it term for term. |
| [14-row-stride-test.png](14-row-stride-test.png) | Four decodes of the same 38×38 texture. Only stride 80 **with** the swap is right — rows are padded to 8 bytes, `((width + 3) & 0xffc) >> 1` u32 words. The padded-stride mip arithmetic then closes to the byte on all 30 textures. |
| [15-torso-textures-zoomed.png](15-torso-textures-zoomed.png) | The torso textures blown up. Confirms the white chest diamond is a real armour plate with a seam, not a UV bug. |
| [16-odd-sized-textures.png](16-odd-sized-textures.png) | The non-square textures: boot, studded strap, face, shoulder patch. No shear, so the padding is between mip levels, not inside rows. |

## Chasing down the alien's legs

Elvis shipped briefly with a flat fin jutting sideways from his hips. It was reported
from the contact sheet, not by any check.

| Image | What it shows |
|---|---|
| [19-elvis-fin-fixed.png](19-elvis-fin-fixed.png) | Before and after. |
| [17-elvis-fin-by-primitive.png](17-elvis-fin-by-primitive.png) | Each primitive in its own colour, which is what located the fin in the hip/thigh batch (cyan) rather than in the legs. |
| [18-blend-frame-candidates.png](18-blend-frame-candidates.png) | Four candidate blend-matrix frames. The leftmost — parent × T(pos) × half-rotation — is clean, which *cleared* the blend maths and pointed at the rig instead. |

The cause: PD puts its blend matrices on **different bones per character** (51 use
elbows + knees; Elvis uniquely uses shoulders + hips). The rig only declared the blends
a body actually used, so Elvis's hip blends had no channel in a clip exported from
a51guard's rig, and stayed unrotated. All 15 blend joints are now declared on every rig
and in every clip. Bug 6 in the handoff.
