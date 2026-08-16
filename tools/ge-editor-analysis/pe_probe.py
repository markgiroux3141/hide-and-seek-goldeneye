import struct, sys, os

PATH = r"C:\GEEdit4\PerfectGold.exe"
data = open(PATH, "rb").read()

pe = struct.unpack_from("<I", data, 0x3C)[0]
nsec = struct.unpack_from("<H", data, pe + 6)[0]
optsz = struct.unpack_from("<H", data, pe + 20)[0]
secoff = pe + 24 + optsz

sections = []
for i in range(nsec):
    o = secoff + i * 40
    name = data[o:o+8].rstrip(b"\0").decode("ascii", "replace")
    vs, va, rs, rp = struct.unpack_from("<IIII", data, o + 8)
    sections.append((name, va, vs, rp, rs))

def rva2off(rva):
    for name, va, vs, rp, rs in sections:
        if va <= rva < va + max(vs, rs):
            return rp + (rva - va)
    return None

dd = pe + 24 + 112

def dirent(i):
    return struct.unpack_from("<II", data, dd + i * 8)

print("=" * 70)
print("DEBUG DIRECTORY / PDB PATH")
print("=" * 70)
rva, sz = dirent(6)
off = rva2off(rva)
for i in range(sz // 28):
    o = off + i * 28
    _, _, _, dtype, dsz, _, draw = struct.unpack_from("<IIHHIII", data, o)
    print(f"  entry type={dtype} size={dsz}")
    if dtype == 2:  # CODEVIEW
        cv = data[draw:draw+dsz]
        if cv[:4] == b"RSDS":
            guid = cv[4:20].hex()
            age = struct.unpack_from("<I", cv, 20)[0]
            pdb = cv[24:].split(b"\0")[0].decode("utf-8", "replace")
            print(f"    RSDS guid={guid} age={age}")
            print(f"    PDB: {pdb}")

print()
print("=" * 70)
print("IMPORTS")
print("=" * 70)
rva, sz = dirent(1)
off = rva2off(rva)
i = 0
while True:
    o = off + i * 20
    oft, ts, fc, nameRva, fthunk = struct.unpack_from("<IIIII", data, o)
    if nameRva == 0:
        break
    no = rva2off(nameRva)
    dll = data[no:data.index(b"\0", no)].decode("ascii", "replace")
    # count functions
    t = rva2off(oft or fthunk)
    funcs = []
    while True:
        v = struct.unpack_from("<Q", data, t)[0]
        if v == 0:
            break
        if not (v >> 63):
            fo = rva2off(v & 0xFFFFFFFF)
            if fo:
                fname = data[fo+2:data.index(b"\0", fo+2)].decode("ascii", "replace")
                funcs.append(fname)
        else:
            funcs.append(f"#{v & 0xFFFF}")
        t += 8
    print(f"  {dll}  ({len(funcs)} imports)")
    print(f"      {', '.join(funcs[:14])}{' ...' if len(funcs) > 14 else ''}")
    i += 1
