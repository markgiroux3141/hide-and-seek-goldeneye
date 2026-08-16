#!/usr/bin/env python3
"""Perfect Dark texture decoder — the compressed global pool.

Character models reference textures two ways. The four bodies that store them
*inline* are handled in `pd_gltf.py` (already-decompressed `RGBA5551`). Everything
else — 62 of the 66 bodies and **all 76 head models** — indexes the global pool in
`textures/`, whose files are compressed. This module decodes those.

Ported from `game/texdecompress.c` + `game/texreset.c` (`tex_read_bits`) and
`lib/rzip.s`.

# File layout

    [flags byte][payload]

The flags byte (`tex_load`, `texdecompress.c:2141`):

    bit 7   hasloddata   — the payload carries its own mip images
    bit 6   iszlib       — which inflate path
    bits 0-5 numlods     — clamped to 5

## The `iszlib` path (2,886 of 3,502 textures)

A bit stream (MSB-first, `tex_read_bits`), which in practice stays byte-aligned:

    u8  format        — TEXFORMAT_* (constants.h:4349)
    u8  numcolours-1
    u16 palette[numcolours]
    per image:
        u8 width, u8 height
        an rzip stream: 0x11 0x73, u24 uncompressed length, then raw DEFLATE

Every texture on this path is **paletted** — 2,462 `RGBA16_CI4`, 417
`RGBA16_CI8`, 7 `IA16_CI8` — which is why `tex_align_indices` only ever handles
the CI cases (its `indicesperbyte` is left uninitialised for the others).

**The inflated data is linear.** `tex_swizzle` and the 8-byte row padding are
applied *after* inflation, to put the image into the layout the RDP reads — so
unlike the inline textures, nothing here needs unswizzling. Getting that backwards
costs you a fine vertical comb over every face; see `HANDOFF_PD_ASSETS.md` bug 7.

## The non-`iszlib` path (616 textures)

`tex_inflate_non_zlib` — a different codec entirely (Huffman + RLE + lookup
tables, `texdecompress.c:699-1930`). **Not ported yet**; [`decode`] raises for
those, and the caller falls back.

Usage:
    python pd_tex.py info  <texture.bin>
    python pd_tex.py sheet <out.png> <texture.bin> [...]
"""

from __future__ import annotations

import argparse
import os
import struct
import sys
import zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

# TEXFORMAT_* (constants.h:4349)
FMT_RGBA32, FMT_RGBA16, FMT_RGB24, FMT_RGB15 = 0, 1, 2, 3
FMT_IA16, FMT_IA8, FMT_IA4, FMT_I8, FMT_I4 = 4, 5, 6, 7, 8
FMT_RGBA16_CI8, FMT_RGBA16_CI4, FMT_IA16_CI8, FMT_IA16_CI4 = 9, 10, 11, 12

FORMAT_NAMES = {
    FMT_RGBA32: "RGBA32", FMT_RGBA16: "RGBA16", FMT_RGB24: "RGB24", FMT_RGB15: "RGB15",
    FMT_IA16: "IA16", FMT_IA8: "IA8", FMT_IA4: "IA4", FMT_I8: "I8", FMT_I4: "I4",
    FMT_RGBA16_CI8: "RGBA16_CI8", FMT_RGBA16_CI4: "RGBA16_CI4",
    FMT_IA16_CI8: "IA16_CI8", FMT_IA16_CI4: "IA16_CI4",
}

#: Palette-index width, per `tex_align_indices`.
INDICES_PER_BYTE = {
    FMT_RGBA16_CI8: 1, FMT_IA16_CI8: 1,
    FMT_RGBA16_CI4: 2, FMT_IA16_CI4: 2,
}
#: Which formats read their 16-bit palette entries as IA16 rather than RGBA5551.
IA_PALETTE = {FMT_IA16_CI8, FMT_IA16_CI4}


class UnsupportedTexture(Exception):
    """A texture this module cannot decode yet (the non-zlib codec)."""


class BitReader:
    """`tex_read_bits` (`texreset.c:21`) — MSB-first over a byte string."""

    def __init__(self, data: bytes):
        self.data = data
        self.pos = 0
        self.acc = 0
        self.nbits = 0

    def read(self, want: int) -> int:
        while self.nbits < want:
            if self.pos >= len(self.data):
                raise UnsupportedTexture("bit stream ran out")
            self.acc = (self.acc << 8) | self.data[self.pos]
            self.pos += 1
            self.nbits += 8
        self.nbits -= want
        return (self.acc >> self.nbits) & ((1 << want) - 1)

    @property
    def byte_pos(self) -> int:
        """Next unread byte. Only meaningful while the stream is byte-aligned,
        which it is for the whole header (8/8/16/8/8-bit fields)."""
        if self.nbits % 8:
            raise UnsupportedTexture("bit stream is not byte-aligned")
        return self.pos - self.nbits // 8


def rzip_inflate(data: bytes, off: int) -> tuple[bytes, int]:
    """Inflate one PD rzip stream at `off`; returns `(bytes, next offset)`.

    `lib/rzip.s:223`: PD's format is `0x11 0x73`, then a 3-byte uncompressed
    length, then raw DEFLATE. (GoldenEye's `0x11 0x72` omits the length.) The
    consumed length is recovered from the decompressor rather than stored, which
    is what lets the caller walk on to the next mip image.
    """
    if data[off] != 0x11 or data[off + 1] != 0x73:
        raise UnsupportedTexture(
            f"expected an rzip 1173 stream at {off}, found {data[off]:#04x} {data[off+1]:#04x}"
        )
    outlen = (data[off + 2] << 16) | (data[off + 3] << 8) | data[off + 4]
    d = zlib.decompressobj(-15)  # raw DEFLATE, no zlib/gzip wrapper
    out = d.decompress(data[off + 5 :], outlen)
    if len(out) < outlen:
        raise UnsupportedTexture(f"rzip stream produced {len(out)} of {outlen} bytes")
    consumed = len(data) - off - 5 - len(d.unused_data)
    return out[:outlen], off + 5 + consumed


def rgba5551(v: int) -> tuple[int, int, int, int]:
    """N64 `RGBA16`: `rrrrrgggggbbbbba`. The 5-bit channels scale by `*255//31`
    so full scale stays full scale — `<<3` would cap white at 248 and grey the
    whole texture down."""
    return (
        ((v >> 11) & 31) * 255 // 31,
        ((v >> 6) & 31) * 255 // 31,
        ((v >> 1) & 31) * 255 // 31,
        255 if v & 1 else 0,
    )


def ia16(v: int) -> tuple[int, int, int, int]:
    """N64 `IA16`: 8 bits intensity, 8 bits alpha."""
    i = (v >> 8) & 0xFF
    return (i, i, i, v & 0xFF)


class PoolTexture:
    """One decoded pool texture: level 0 as RGBA8, plus what it was."""

    __slots__ = ("width", "height", "format", "rgba", "numlods", "hasloddata", "numcolours")

    def __init__(self, width, height, fmt, rgba, numlods, hasloddata, numcolours):
        self.width = width
        self.height = height
        self.format = fmt
        self.rgba = rgba
        self.numlods = numlods
        self.hasloddata = hasloddata
        self.numcolours = numcolours

    @property
    def format_name(self) -> str:
        return FORMAT_NAMES.get(self.format, f"?{self.format}")


def decode(data: bytes) -> PoolTexture:
    """Decode a `textures/*.bin` to level 0 as tightly-packed RGBA8.

    Raises [`UnsupportedTexture`] for the non-zlib codec, which is not ported.
    """
    if len(data) < 2:
        raise UnsupportedTexture("empty texture")
    flags = data[0]
    hasloddata = bool(flags & 0x80)
    iszlib = bool(flags & 0x40)
    numlods = min(flags & 0x3F, 5)
    if not iszlib:
        raise UnsupportedTexture("non-zlib codec (tex_inflate_non_zlib) is not ported")

    payload = data[1:]
    br = BitReader(payload)
    fmt = br.read(8)
    numcolours = br.read(8) + 1
    palette = [br.read(16) for _ in range(numcolours)]

    ipb = INDICES_PER_BYTE.get(fmt)
    if ipb is None:
        # Every zlib-path texture in the shipped set is paletted; a non-CI one
        # here would mean `tex_align_indices` running on uninitialised state.
        raise UnsupportedTexture(f"zlib path with non-paletted format {FORMAT_NAMES.get(fmt, fmt)}")

    # Only level 0 is wanted; the engine regenerates the rest.
    off = br.byte_pos
    width, height = payload[off], payload[off + 1]
    indices, _ = rzip_inflate(payload, off + 2)

    to_rgba = ia16 if fmt in IA_PALETTE else rgba5551
    lut = [to_rgba(v) for v in palette]
    stride = (width + ipb - 1) // ipb  # packed rows; padding is applied later by
    #                                     `tex_align_indices`, not here
    need = stride * height
    if len(indices) < need:
        raise UnsupportedTexture(f"inflated {len(indices)} bytes, need {need} for {width}x{height}")

    px = bytearray(width * height * 4)
    for y in range(height):
        row = y * stride
        for x in range(width):
            if ipb == 2:
                b = indices[row + (x >> 1)]
                idx = (b >> 4) if (x & 1) == 0 else (b & 0xF)
            else:
                idx = indices[row + x]
            r, g, bl, a = lut[idx] if idx < len(lut) else (255, 0, 255, 255)
            d = (y * width + x) * 4
            px[d] = r
            px[d + 1] = g
            px[d + 2] = bl
            px[d + 3] = a
    return PoolTexture(width, height, fmt, bytes(px), numlods, hasloddata, numcolours)


def assets_root() -> str:
    return os.path.join(
        os.path.dirname(os.path.abspath(__file__)),
        "..", "..", "reference", "pd-decomp", "src", "assets", "ntsc-final",
    )


def load(texturenum: int) -> PoolTexture:
    """Decode pool texture `texturenum` (as `textureconfig.texturenum` gives it)."""
    path = os.path.join(assets_root(), "textures", f"{texturenum:04x}.bin")
    with open(path, "rb") as fh:
        return decode(fh.read())


# ---------------------------------------------------------------------------


def cmd_info(paths) -> int:
    ok = bad = 0
    for p in paths:
        with open(p, "rb") as fh:
            data = fh.read()
        try:
            t = decode(data)
            ok += 1
            print(
                f"{os.path.basename(p):<12} {t.width:3}x{t.height:<3} {t.format_name:<11} "
                f"{t.numcolours:3} colours  lods={t.numlods} hasloddata={int(t.hasloddata)}"
            )
        except UnsupportedTexture as e:
            bad += 1
            print(f"{os.path.basename(p):<12} UNSUPPORTED: {e}")
    print(f"\n{ok} decoded, {bad} unsupported")
    return 0


def cmd_sheet(out: str, paths, cell: int = 80) -> int:
    from pd_gltf import png_bytes  # noqa: E402

    tiles = []
    for p in paths:
        with open(p, "rb") as fh:
            data = fh.read()
        try:
            tiles.append((os.path.basename(p), decode(data)))
        except UnsupportedTexture:
            continue
    if not tiles:
        raise SystemExit("nothing decoded")
    cols = min(8, len(tiles))
    rows = (len(tiles) + cols - 1) // cols
    tw, th = cols * cell, rows * cell
    sheet = bytearray()
    for _ in range(tw * th):
        sheet += bytes((30, 32, 36, 255))
    for i, (_name, t) in enumerate(tiles):
        cx, cy = (i % cols) * cell, (i // cols) * cell
        sc = min((cell - 8) / t.width, (cell - 8) / t.height)
        dw, dh = max(int(t.width * sc), 1), max(int(t.height * sc), 1)
        ox, oy = cx + (cell - dw) // 2, cy + (cell - dh) // 2
        for y in range(dh):
            for x in range(dw):
                s = (int(y / sc) * t.width + int(x / sc)) * 4
                d = ((oy + y) * tw + (ox + x)) * 4
                if t.rgba[s + 3] == 0:
                    v = 95 if ((x // 6 + y // 6) % 2) else 60
                    sheet[d : d + 4] = bytes((v, v, v, 255))
                else:
                    sheet[d : d + 3] = t.rgba[s : s + 3]
    with open(out, "wb") as fh:
        fh.write(png_bytes(tw, th, bytes(sheet)))
    print(f"{len(tiles)} textures -> {out}")
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("info", help="decode and summarise")
    p.add_argument("textures", nargs="+")
    p = sub.add_parser("sheet", help="render a contact sheet")
    p.add_argument("out")
    p.add_argument("textures", nargs="+")
    p.add_argument("--cell", type=int, default=80)
    args = ap.parse_args()
    if args.cmd == "info":
        return cmd_info(args.textures)
    return cmd_sheet(args.out, args.textures, args.cell)


if __name__ == "__main__":
    sys.exit(main())
