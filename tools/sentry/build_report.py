#!/usr/bin/env python3
"""Build the auto-turret teardown page from the rendered previews.

Every image in `preview/` is inlined as a data URI, so the page is self-contained and
can be published as an Artifact (which blocks every external host). Re-run after
re-rendering to refresh the page in place.

    python build_report.py <out.html>
"""

from __future__ import annotations

import base64
import html
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PREVIEW = os.path.join(HERE, "preview")


def data_uri(name):
    with open(os.path.join(PREVIEW, name), "rb") as fh:
        return "data:image/png;base64," + base64.b64encode(fh.read()).decode("ascii")


#: (stage no, title, dek, [(image, caption)], [finding paragraphs])
STAGES = [
    (
        "01",
        "What ships in the box",
        "The turret is already in the catalog as <code>sentry_gun</code> — a "
        "GoldenEye Setup Editor export (OBJ + MTL + BMP), 97 vertices and 57 "
        "triangles, drawn today as one static lump. First job: look at it.",
        [("01_textured.png", "The raw asset, drawn exactly as the engine draws it "
                             "— Kd tint times BMP texel, unlit, six orthographic views.")],
        ["A 6-barrel gatling bundle, a housing, a vented cowl and a mounting fin are "
         "all clearly present. They are also clearly not attached to each other."],
    ),
    (
        "02",
        "It is not a turret, it is a parts sheet",
        "Welding the vertices and taking connected components splits the mesh into "
        "six disjoint pieces — and they are parked in a row rather than assembled.",
        [("02_parts.png", "Six connected components, one flat colour each. The gaps "
                          "between them are the finding.")],
        ["The pieces sit on three shelves along Z, at −325 / 0 / +325 GoldenEye units. "
         "Each shelf is internally correct — the barrel bundle really does abut the "
         "housing's +X face — but the shelves were never joined. The editor exported "
         "each part at its own local origin and left the assembly to the game.",
         "The other editor props in the catalog (security camera, grey table, body "
         "armour) are coherent single objects, so this is specific to the turret, not "
         "a quirk of the exporter. There is no skeleton to recover: the rig is ours "
         "to author."],
    ),
    (
        "03",
        "Naming the six pieces",
        "Framed one at a time, on their own bounds, each component becomes obvious.",
        [("03_components_iso.png", "Each component framed on itself, with its "
                                   "measured size in metres and its centre in GE units.")],
        ["Reading them in order: the <b>barrel bundle</b> (0.60 m, six barrels, muzzle "
         "at +X — which fixes the bore axis), the <b>housing</b> (0.80 × 0.50 × 0.25 m), "
         "a vented <b>cowl</b> with a translucent dish decal inside it, a vertical "
         "<b>fin</b>, a long thin <b>trunnion</b> prism, and a flat panel.",
         "That maps onto a hanging turret four nodes deep: cowl bolted to the ceiling, "
         "fin dropping from it as the yaw shaft, trunnion as the pitch axle, housing "
         "swinging on the trunnion, bundle spinning on the bore."],
    ),
    (
        "04",
        "Assembled",
        "Three shelves slid together, the fin stretched 1.24× in Y to close the gap "
        "between the ceiling plate and the trunnion, and the whole thing scaled to a "
        "room that is 2.0 m tall.",
        [("08_rig_v2.png", "The assembled turret at rest."),
         ("06_rig_nodes.png", "The same assembly coloured by rig node: "
                              "mount, yaw, pitch, spin.")],
        ["At the raw export scale the turret is 1.40 m long and hangs 1.05 m — in a "
         "2.0 m room that puts a gatling gun at head height, the same N64-scale trap "
         "the door props hit. At 0.45 it is a 0.63 m gun hanging 0.47 m: something you "
         "duck under.",
         "The first placement attempt was wrong and the render caught it. Hanging the "
         "gun from a pivot at its top-front corner — the obvious reading of \"the shaft "
         "holds the gun\" — made the 0.8 m housing scythe up through the ceiling plate "
         "the moment it pitched down. The trunnion belongs on the <b>bore line</b>, "
         "with the housing balanced across it."],
    ),
    (
        "05",
        "Pitch limits, checked against the mount",
        "The useful question is not \"how far can it pitch\" but \"how far before it "
        "hits the thing it hangs from\".",
        [("09_pitch_min.png", "Pitch −50°, the clamped floor. The housing's back "
                              "corner rises but stays under the ceiling plate."),
         ("10_pitch_max.png", "Pitch +15°, the clamped ceiling.")],
        ["Swept over the whole clamp range, the housing's highest corner peaks at "
         "Y = −296 GE units against a ceiling plate whose underside is at −200: "
         "<b>96 units of clearance</b>, 0.043 m at the shipping scale. Unclamped it "
         "would foul, which is what the clamp is for.",
         "−50° also covers the room. The bore sits about 1.69 m above the floor, so "
         "−50° reaches a target's chest half a metre away; anything further is a "
         "shallower shot."],
    ),
    (
        "06",
        "Articulation",
        "Yaw carries pitch carries spin, composed parent-last — the same order the "
        "Rust side will use.",
        [("07_anim_iso.png", "A tracking sweep: −60° to +60° of yaw while pitching "
                             "down and spinning up."),
         ("11_spin.png", "One sixth-turn of the bundle, close on the muzzle. Six "
                         "barrels, so 60° is a full visual cycle.")],
        ["The hex face steps round evenly with no wobble, which is the check that the "
         "spin axis is actually on the bore and not merely near it. A spin axis off "
         "the bore reads as a bent barrel the instant it turns."],
    ),
]

STAGES.append((
    "07",
    "The same rig, this time from the engine",
    "Everything above is a rig written in Python. The game runs a second, independent "
    "transcription of those numbers into Rust and glam — and the two could disagree in "
    "ways no unit test notices. So the engine dumps what <i>it</i> computed, and that "
    "gets drawn through the same renderer.",
    [("13_engine_tracking.png", "Posed by <code>crates/game/src/turret.rs</code> at "
                                "yaw &minus;35&deg;, pitch &minus;25&deg;, barrels 25&deg; "
                                "round — dumped to a Wavefront OBJ and rendered.")],
    ["The two agree to the digit. Every piece's bounding box lands where the Python rig "
     "puts it: the bundle at X&nbsp;[400,&nbsp;1000], the housing at X&nbsp;[&minus;400,&nbsp;400] "
     "balanced across the trunnion, the plate at Y&nbsp;[&minus;200,&nbsp;0].",
     "That matters because a transposed multiply or a flipped rotation sign reads as "
     "\"the barrel points backwards\", not as an assertion failure. Five unit tests "
     "cover the parts an assertion <i>can</i> catch — that solving for an aim direction "
     "is genuinely the inverse of pointing there, that the muzzle rides the bore, that "
     "the pitch clamp keeps the housing clear of the plate."],
))

NEXT = [
    ("Ceiling only", "The turret is placed against a ceiling, not a floor, and hangs "
                     "from its mount point. Aim at an overhead face in BUILD and the "
                     "ghost hangs below the cursor."),
    ("It spins up before it hurts", "Half a second of barrel spin before the first "
                                    "round leaves, and a slow coast down after. That "
                                    "whir is the warning."),
    ("RC-P90 rounds", "10 damage on the game's own bullet-hose cadence of 0.07 s — "
                      "about 143 damage a second against a hunter's 100 health, so "
                      "roughly a second on target once it is spun up."),
    ("Still to do", "The flash is an impact spark at the barrel tip rather than the "
                    "real muzzle-flash billboard, and hunters do not yet react to "
                    "being shot by something that is not the player."),
]


def figure(img, cap):
    return (
        f'<figure>\n<div class="plate"><img src="{data_uri(img)}" alt="{html.escape(cap)}" />'
        f'</div>\n<figcaption>{cap}</figcaption>\n</figure>'
    )


def build():
    stages = []
    for num, title, dek, imgs, notes in STAGES:
        figs = "\n".join(figure(i, c) for i, c in imgs)
        note_html = "\n".join(f"<p>{n}</p>" for n in notes)
        stages.append(f"""<section class="stage">
  <div class="rail"><span class="num">{num}</span></div>
  <div class="body">
    <h2>{title}</h2>
    <p class="dek">{dek}</p>
    {figs}
    <div class="note">{note_html}</div>
  </div>
</section>""")

    nexts = "\n".join(
        f'<li><h3>{t}</h3><p>{d}</p></li>' for t, d in NEXT
    )

    return f"""<title>Auto-turret teardown</title>
<style>
:root {{
  --ground: #f2f3f5;
  --surface: #ffffff;
  --plate: #1a1d23;
  --ink: #15181d;
  --dim: #5d6672;
  --faint: #8b95a3;
  --edge: #d8dce2;
  --steel: #3d6d8c;
  --amber: #a06a12;
  --mono: ui-monospace, "SF Mono", "Cascadia Mono", Menlo, Consolas, monospace;
  --sans: system-ui, -apple-system, "Segoe UI", Roboto, sans-serif;
}}
@media (prefers-color-scheme: dark) {{
  :root {{
    --ground: #101317; --surface: #171b21; --plate: #0c0e12;
    --ink: #e6e9ee; --dim: #98a3b1; --faint: #6b7686; --edge: #262c35;
    --steel: #7fb2d1; --amber: #d9a441;
  }}
}}
:root[data-theme="dark"] {{
  --ground: #101317; --surface: #171b21; --plate: #0c0e12;
  --ink: #e6e9ee; --dim: #98a3b1; --faint: #6b7686; --edge: #262c35;
  --steel: #7fb2d1; --amber: #d9a441;
}}
:root[data-theme="light"] {{
  --ground: #f2f3f5; --surface: #ffffff; --plate: #1a1d23;
  --ink: #15181d; --dim: #5d6672; --faint: #8b95a3; --edge: #d8dce2;
  --steel: #3d6d8c; --amber: #a06a12;
}}

* {{ box-sizing: border-box; }}
body {{
  margin: 0; background: var(--ground); color: var(--ink);
  font-family: var(--sans); font-size: 16px; line-height: 1.65;
  -webkit-font-smoothing: antialiased;
}}
.wrap {{ max-width: 1180px; margin: 0 auto; padding: 0 24px 96px; }}

header.top {{ padding: 72px 0 40px; border-bottom: 1px solid var(--edge); }}
.eyebrow {{
  font-family: var(--mono); font-size: 11px; letter-spacing: .18em;
  text-transform: uppercase; color: var(--amber); margin: 0 0 18px;
}}
h1 {{
  font-family: var(--mono); font-weight: 700; font-size: clamp(30px, 5.5vw, 52px);
  line-height: 1.04; letter-spacing: -.02em; margin: 0 0 20px;
  text-wrap: balance; max-width: 18ch;
}}
h1 em {{ font-style: normal; color: var(--steel); }}
.standfirst {{ max-width: 64ch; font-size: 18px; color: var(--dim); margin: 0; }}

.facts {{
  display: grid; gap: 1px; background: var(--edge);
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  border: 1px solid var(--edge); margin: 40px 0 0;
}}
.facts div {{ background: var(--surface); padding: 16px 18px; }}
.facts dt {{
  font-family: var(--mono); font-size: 10px; letter-spacing: .14em;
  text-transform: uppercase; color: var(--faint); margin: 0 0 6px;
}}
.facts dd {{
  margin: 0; font-family: var(--mono); font-size: 20px; font-weight: 600;
  font-variant-numeric: tabular-nums;
}}

.stage {{
  display: grid; grid-template-columns: 72px minmax(0, 1fr); gap: 0 24px;
  padding: 56px 0; border-bottom: 1px solid var(--edge);
}}
.rail {{ position: sticky; top: 24px; align-self: start; }}
.num {{
  font-family: var(--mono); font-size: 13px; font-weight: 700;
  letter-spacing: .1em; color: var(--amber);
  display: block; padding-top: 6px; border-top: 2px solid var(--amber);
}}
.body {{ min-width: 0; }}
h2 {{
  font-family: var(--mono); font-size: clamp(21px, 2.6vw, 28px); font-weight: 700;
  letter-spacing: -.015em; margin: 0 0 12px; text-wrap: balance;
}}
.dek {{ max-width: 66ch; color: var(--dim); margin: 0 0 28px; }}

figure {{ margin: 0 0 22px; }}
.plate {{
  background: var(--plate); border: 1px solid var(--edge);
  overflow-x: auto; padding: 10px;
}}
.plate img {{ display: block; max-width: 100%; height: auto; min-width: 460px; }}
figcaption {{
  font-family: var(--mono); font-size: 12px; line-height: 1.55;
  color: var(--faint); margin-top: 9px; max-width: 78ch;
}}

.note {{
  border-left: 2px solid var(--steel); padding-left: 18px; margin-top: 26px;
  max-width: 66ch;
}}
.note p {{ margin: 0 0 12px; }}
.note p:last-child {{ margin-bottom: 0; }}
.note b {{ color: var(--steel); font-weight: 600; }}

code {{
  font-family: var(--mono); font-size: .88em;
  background: color-mix(in srgb, var(--steel) 14%, transparent);
  padding: 1px 5px; border-radius: 2px;
}}

.next {{ padding: 56px 0 0; }}
.next h2 {{ margin-bottom: 28px; }}
.next ul {{
  list-style: none; margin: 0; padding: 0; display: grid; gap: 1px;
  grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
  background: var(--edge); border: 1px solid var(--edge);
}}
.next li {{ background: var(--surface); padding: 20px; }}
.next h3 {{
  font-family: var(--mono); font-size: 13px; font-weight: 700;
  letter-spacing: .04em; margin: 0 0 8px; color: var(--steel);
}}
.next p {{ margin: 0; font-size: 14.5px; color: var(--dim); }}

@media (max-width: 680px) {{
  .stage {{ grid-template-columns: 1fr; gap: 14px; }}
  .rail {{ position: static; }}
  .num {{ display: inline-block; padding-right: 40px; }}
}}
</style>

<div class="wrap">
<header class="top">
  <p class="eyebrow">Build log &middot; native / props</p>
  <h1>Making the <em>sentry gun</em> actually shoot</h1>
  <p class="standfirst">A GoldenEye auto-turret has been sitting in the prop catalog
  as scenery. Before it can track a target and spin its barrels, it has to be taken
  apart — and it turns out it was never put together. Every image below is rendered on
  the CPU straight from the source asset, with no build and no GPU.</p>
  <dl class="facts">
    <div><dt>Source</dt><dd>57 tris</dd></div>
    <div><dt>Pieces found</dt><dd>6</dd></div>
    <div><dt>Rig nodes</dt><dd>4</dd></div>
    <div><dt>Assembled</dt><dd>0.63 m</dd></div>
    <div><dt>Pitch range</dt><dd>&minus;50&deg;&hairsp;/&hairsp;+15&deg;</dd></div>
  </dl>
</header>

{chr(10).join(stages)}

<section class="next">
  <h2>Where it stands</h2>
  <ul>{nexts}</ul>
</section>
</div>
"""


if __name__ == "__main__":
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(HERE, "teardown.html")
    with open(out, "w", encoding="utf-8") as fh:
        fh.write(build())
    print(f"wrote {out}  ({os.path.getsize(out) / 1024:.0f} KB)")
