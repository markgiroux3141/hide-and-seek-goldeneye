import struct, zlib, collections

ROM = r"D:\GoldenPerfectModding\007 - GoldenEye (USA)\007 - GoldenEye (USA).n64"
raw = open(ROM, "rb").read()
b = bytearray(raw)
b[0::2], b[1::2] = raw[1::2], raw[0::2]
data = bytes(b)
assert data[:4] == bytes.fromhex("80371240")

hits = []
pos = 0
while True:
    pos = data.find(b"\x11\x72", pos)
    if pos < 0:
        break
    if pos % 2 == 0:
        hits.append(pos)
    pos += 2

print(f"aligned 0x1172 markers: {len(hits):,}\n")

# sweep: where does the deflate stream start relative to the marker?
results = collections.Counter()
detail = collections.defaultdict(list)
for h in hits:
    for delta in range(2, 12):
        body = data[h + delta: h + delta + 0x30000]
        try:
            out = zlib.decompressobj(-15).decompress(body)
        except Exception:
            continue
        if len(out) >= 512:
            results[delta] += 1
            if len(detail[delta]) < 5:
                detail[delta].append((h, len(out), out[:12].hex().upper()))

print("deflate-start offset from marker -> count of successful inflations (>=512 bytes)")
for d, c in sorted(results.items()):
    print(f"  marker+{d:<3} : {c:,}")

best = max(results, key=results.get) if results else None
if best:
    print(f"\nBEST = marker+{best}   ({results[best]:,} successes of {len(hits):,} markers)")
    print("  samples (romOff, inflatedLen, first bytes):")
    for h, ln, pre in detail[best]:
        hdr = data[h:h+best].hex().upper()
        print(f"    0x{h:07X} hdr={hdr}  -> {ln:,} bytes  {pre}")
