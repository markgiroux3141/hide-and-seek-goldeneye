import struct, os, sys

def be32(b, o): return struct.unpack_from(">I", b, o)[0]
def be16(b, o): return struct.unpack_from(">H", b, o)[0]

def hexdump(b, off, n, base=0):
    out = []
    for i in range(0, n, 16):
        chunk = b[off+i:off+i+16]
        if not chunk: break
        h = " ".join(f"{c:02X}" for c in chunk)
        a = "".join(chr(c) if 32 <= c < 127 else "." for c in chunk)
        out.append(f"  {base+off+i:06X}  {h:<47}  {a}")
    return "\n".join(out)

# ---------------- GE SETUP ----------------
p = r"C:\GEEdit4\GE\Setup\UsetupdamZ.set"
b = open(p, "rb").read()
print("=" * 72)
print(f"GE SETUP: {os.path.basename(p)}   size={len(b):,} (0x{len(b):X})")
print("=" * 72)
print("header (first 0x60):")
print(hexdump(b, 0, 0x60))
print("\n  interpreted as 9 BE u32 pointers (GE setup header):")
for i in range(9):
    v = be32(b, i * 4)
    print(f"    [{i}] +0x{i*4:02X} = 0x{v:08X}  ({'in-range' if v < len(b) else 'OUT'})")

# ---------------- PD SETUP ----------------
p2 = r"C:\GEEdit4\PD\Setup\Ump_setupoatZ.set"
if not os.path.exists(p2):
    import glob
    c = glob.glob(r"C:\GEEdit4\PD\**\*.set", recursive=True)
    p2 = c[0] if c else None
if p2:
    b2 = open(p2, "rb").read()
    print("\n" + "=" * 72)
    print(f"PD SETUP: {p2}   size={len(b2):,}")
    print("=" * 72)
    print(hexdump(b2, 0, 0x50))

# ---------------- GE BGF ----------------
p3 = r"C:\GEEdit4\GE\BGDataFull\449450.bgf"
b3 = open(p3, "rb").read()
print("\n" + "=" * 72)
print(f"GE BG FILE: {os.path.basename(p3)}  size={len(b3):,} (0x{len(b3):X})")
print("=" * 72)
print(hexdump(b3, 0, 0x60))
print("\n  first 12 BE u32:")
for i in range(12):
    print(f"    +0x{i*4:02X} = 0x{be32(b3,i*4):08X}")

# ---------------- PD CLIPPING ----------------
p4 = r"C:\GEEdit4\PD\pdclipping\bg_azt_tiles.clp"
b4 = open(p4, "rb").read()
print("\n" + "=" * 72)
print(f"PD CLIPPING (tiles): {os.path.basename(p4)}  size={len(b4):,}")
print("=" * 72)
print(hexdump(b4, 0, 0x60))
