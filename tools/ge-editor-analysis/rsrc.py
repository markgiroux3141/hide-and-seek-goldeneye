import struct, sys, os, json, collections

PATH = r"C:\GEEdit4\PerfectGold.exe"
OUT = os.path.dirname(os.path.abspath(__file__))
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
res_rva, res_sz = struct.unpack_from("<II", data, dd + 2 * 8)
RES = rva2off(res_rva)

TYPES = {1:"CURSOR",2:"BITMAP",3:"ICON",4:"MENU",5:"DIALOG",6:"STRING",7:"FONTDIR",
         8:"FONT",9:"ACCELERATOR",10:"RCDATA",11:"MESSAGETABLE",12:"GROUP_CURSOR",
         14:"GROUP_ICON",16:"VERSION",24:"MANIFEST"}

def read_dir(off, level, path, out):
    nnamed, nid = struct.unpack_from("<HH", data, off + 12)
    for i in range(nnamed + nid):
        eo = off + 16 + i * 8
        nameval, offval = struct.unpack_from("<II", data, eo)
        if nameval & 0x80000000:
            no = RES + (nameval & 0x7FFFFFFF)
            ln = struct.unpack_from("<H", data, no)[0]
            nm = data[no+2:no+2+ln*2].decode("utf-16-le", "replace")
        else:
            nm = nameval
        if offval & 0x80000000:
            read_dir(RES + (offval & 0x7FFFFFFF), level + 1, path + [nm], out)
        else:
            do = RES + offval
            drva, dsz, cp, _ = struct.unpack_from("<IIII", data, do)
            out.append((path + [nm], rva2off(drva), dsz))

entries = []
read_dir(RES, 0, [], entries)

bytype = collections.defaultdict(list)
for path, off, sz in entries:
    t = path[0]
    bytype[TYPES.get(t, str(t)) if isinstance(t, int) else t].append((path, off, sz))

print("=" * 70)
print("RESOURCE INVENTORY")
print("=" * 70)
for k in sorted(bytype, key=lambda x: -len(bytype[x])):
    total = sum(s for _, _, s in bytype[k])
    print(f"  {k:<16} count={len(bytype[k]):<6} bytes={total:,}")

# ---------- STRING TABLE ----------
strings = {}
for path, off, sz in bytype.get("STRING", []):
    blockid = path[1]
    p = off
    end = off + sz
    idx = 0
    while p < end - 1:
        ln = struct.unpack_from("<H", data, p)[0]
        p += 2
        if ln:
            s = data[p:p+ln*2].decode("utf-16-le", "replace")
            strings[(blockid - 1) * 16 + idx] = s
            p += ln * 2
        idx += 1
print(f"\nSTRING TABLE entries: {len(strings)}")
json.dump({str(k): v for k, v in sorted(strings.items())},
          open(os.path.join(OUT, "strtable.json"), "w", encoding="utf-8"),
          indent=1, ensure_ascii=False)

# ---------- MENUS ----------
def parse_menu(off, sz):
    items = []
    p = off
    ver, hdrsz = struct.unpack_from("<HH", data, p)
    p += 4 + hdrsz
    depth = 0
    end = off + sz
    while p < end and depth >= 0:
        flags = struct.unpack_from("<H", data, p)[0]
        p += 2
        popup = flags & 0x10
        if not popup:
            mid = struct.unpack_from("<H", data, p)[0]
            p += 2
        else:
            mid = None
        s = []
        while p < end:
            c = struct.unpack_from("<H", data, p)[0]
            p += 2
            if c == 0:
                break
            s.append(chr(c))
        txt = "".join(s)
        items.append(("  " * depth) + (txt if txt else "---") + (f"   [{mid}]" if mid else ""))
        if popup:
            depth += 1
        if flags & 0x80:  # MF_END
            depth -= 1
            while depth >= 0 and p < end:
                break
    return items

menus = {}
for path, off, sz in bytype.get("MENU", []):
    try:
        menus[str(path[1])] = parse_menu(off, sz)
    except Exception as e:
        menus[str(path[1])] = [f"<parse error {e}>"]
json.dump(menus, open(os.path.join(OUT, "menus.json"), "w", encoding="utf-8"),
          indent=1, ensure_ascii=False)
print(f"MENU resources: {len(menus)}")

# ---------- DIALOGS ----------
def read_sz_or_ord(p):
    v = struct.unpack_from("<H", data, p)[0]
    if v == 0:
        return "", p + 2
    if v == 0xFFFF:
        return f"#{struct.unpack_from('<H', data, p+2)[0]}", p + 4
    s = []
    while True:
        c = struct.unpack_from("<H", data, p)[0]
        p += 2
        if c == 0:
            break
        s.append(chr(c))
    return "".join(s), p

def parse_dialog(off, sz):
    p = off
    sig, ver = struct.unpack_from("<HH", data, p + 2), None
    ver_, sig_ = struct.unpack_from("<HH", data, p)
    ex = (ver_ == 1 and sig_ == 0xFFFF)
    if ex:
        p += 4
        helpid, exstyle, style = struct.unpack_from("<III", data, p); p += 12
        cdit, x, y, cx, cy = struct.unpack_from("<Hhhhh", data, p); p += 10
    else:
        style, exstyle = struct.unpack_from("<II", data, p); p += 8
        cdit, x, y, cx, cy = struct.unpack_from("<Hhhhh", data, p); p += 10
    menu, p = read_sz_or_ord(p)
    cls, p = read_sz_or_ord(p)
    title, p = read_sz_or_ord(p)
    if style & 0x40:  # DS_SETFONT
        if ex:
            p += 6
            _, p = read_sz_or_ord(p)
        else:
            p += 2
            _, p = read_sz_or_ord(p)
    controls = []
    for _ in range(cdit):
        p = (p + 3) & ~3
        if p >= off + sz:
            break
        if ex:
            p += 12
            cid = struct.unpack_from("<I", data, p + 12)[0] if False else None
            cx_, cy_, w_, h_ = struct.unpack_from("<hhhh", data, p); p += 8
            cid = struct.unpack_from("<I", data, p)[0]; p += 4
        else:
            cstyle, cex = struct.unpack_from("<II", data, p); p += 8
            cx_, cy_, w_, h_ = struct.unpack_from("<hhhh", data, p); p += 8
            cid = struct.unpack_from("<H", data, p)[0]; p += 2
        ccls, p = read_sz_or_ord(p)
        ctxt, p = read_sz_or_ord(p)
        extra = struct.unpack_from("<H", data, p)[0]; p += 2 + extra
        controls.append({"id": cid, "cls": ccls, "text": ctxt})
    return {"title": title, "size": [cx, cy], "controls": controls}

dialogs = {}
for path, off, sz in bytype.get("DIALOG", []):
    try:
        dialogs[str(path[1])] = parse_dialog(off, sz)
    except Exception as e:
        dialogs[str(path[1])] = {"error": str(e)}
json.dump(dialogs, open(os.path.join(OUT, "dialogs.json"), "w", encoding="utf-8"),
          indent=1, ensure_ascii=False)
ok = [d for d in dialogs.values() if "error" not in d]
print(f"DIALOG resources: {len(dialogs)}  parsed_ok={len(ok)}")
print(f"\nWrote strtable.json / menus.json / dialogs.json to {OUT}")
