import struct, re, os, sys, collections

PATH = r"C:\GEEdit4\PerfectGold.exe"
OUT = os.path.dirname(os.path.abspath(__file__))
data = open(PATH, "rb").read()

pe = struct.unpack_from("<I", data, 0x3C)[0]
nsec = struct.unpack_from("<H", data, pe + 6)[0]
optsz = struct.unpack_from("<H", data, pe + 20)[0]
secoff = pe + 24 + optsz
sections = {}
for i in range(nsec):
    o = secoff + i * 40
    name = data[o:o+8].rstrip(b"\0").decode("ascii", "replace")
    vs, va, rs, rp = struct.unpack_from("<IIII", data, o + 8)
    sections[name] = (rp, rs)

pat = re.compile(rb"[\x20-\x7E\t]{6,}")

allstr = []
for sec in (".rdata", ".data", ".text"):
    rp, rs = sections[sec]
    blob = data[rp:rp+rs]
    for m in pat.finditer(blob):
        s = m.group().decode("ascii")
        allstr.append((sec, rp + m.start(), s))

print(f"total ascii strings >=6: {len(allstr)}")
with open(os.path.join(OUT, "allstrings.txt"), "w", encoding="utf-8") as f:
    for sec, off, s in allstr:
        f.write(f"{sec}\t0x{off:08X}\t{s}\n")

def grep(label, rx, limit=60, uniq=True):
    r = re.compile(rx, re.I)
    seen = []
    ss = set()
    for sec, off, s in allstr:
        if r.search(s):
            if uniq and s in ss:
                continue
            ss.add(s)
            seen.append(s)
    print(f"\n{'='*68}\n{label}   ({len(seen)} unique)\n{'='*68}")
    for s in seen[:limit]:
        print("  " + s[:150])

grep("FILE EXTENSIONS / FILTERS", r"\*\.\w+")
grep("FORMAT / SEGMENT NAMES", r"\.seg|bgdata/|^bg_|Tbg_|Usetup|Ump_setup")
grep("COMPRESSION", r"rarezip|rare zip|1172|decompress|compress(ed|ion)?\b|inflate|deflate|zlib|gzip")
grep("F3DEX / RSP DISPLAY LIST", r"F3DEX|G_[A-Z]{3,}|gsDP|gsSP|vertex buffer|display list")
grep("N64 IMAGE FORMATS", r"\bRGBA\b|\bIA\b|\bCI\b|I4|I8|IA4|IA8|IA16|RGBA16|RGBA32|CI4|CI8|texel")
