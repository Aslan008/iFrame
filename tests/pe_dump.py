import struct

path = r"C:\Users\mshat\Desktop\iFrame\target\release\d3d9_test_app.exe"
data = open(path, "rb").read()

e_lfanew = struct.unpack_from("<I", data, 0x3C)[0]
assert data[e_lfanew:e_lfanew + 4] == b"PE\0\0"
num_sections = struct.unpack_from("<H", data, e_lfanew + 6)[0]
opt_size = struct.unpack_from("<H", data, e_lfanew + 20)[0]
opt = e_lfanew + 24
magic = struct.unpack_from("<H", data, opt)[0]
kind = "PE32+" if magic == 0x20B else "PE32"
print(f"PE magic: {magic:#x} ({kind}), sections: {num_sections}")

sec_off = opt + opt_size
sections = []
for i in range(num_sections):
    o = sec_off + i * 40
    name = data[o:o + 8].rstrip(b"\0").decode(errors="replace")
    vsize, vaddr, rsize, roff = struct.unpack_from("<IIII", data, o + 8)
    sections.append((name, vaddr, vsize, rsize, roff))
    print(f"  sec {name:10} va=0x{vaddr:x} vs=0x{vsize:x} raw=0x{rsize:x} ro=0x{roff:x}")


def rva2file(rva):
    for name, vaddr, vsize, rsize, roff in sections:
        if vaddr <= rva < vaddr + max(vsize, rsize):
            delta = rva - vaddr
            if delta < rsize:
                return roff + delta
    return None


dd_off = opt + (112 if magic == 0x20B else 96)
imp_rva, imp_size = struct.unpack_from("<II", data, dd_off + 8)
print(f"import dir: rva=0x{imp_rva:x} size=0x{imp_size:x}")

off = rva2file(imp_rva)
if off is None:
    print("rva2file FAILED for import dir")
    raise SystemExit(1)
print(f"import dir file offset: 0x{off:x}")

d = 0
while True:
    base = off + d * 20
    int_rva, ts, fc, name_rva, iat_rva = struct.unpack_from("<IIIII", data, base)
    if name_rva == 0:
        break
    nm_off = rva2file(name_rva)
    dll = data[nm_off:data.index(b"\0", nm_off)].decode() if nm_off else "?"
    print(f"desc[{d}]: dll={dll!r} int=0x{int_rva:x} iat=0x{iat_rva:x}")
    if dll.lower() == "d3d9.dll" and int_rva:
        i = 0
        int_off = rva2file(int_rva)
        while True:
            t = struct.unpack_from("<Q", data, int_off + i * 8)[0]
            if t == 0:
                break
            if not (t & 0x8000000000000000):
                no = rva2file(t & 0xFFFFFFFF)
                hint = struct.unpack_from("<H", data, no)[0]
                fname = data[no + 2:data.index(b"\0", no + 2)].decode()
                print(f"  thunk[{i}]: {fname!r} (hint={hint})")
            else:
                print(f"  thunk[{i}]: ORDINAL {t & 0xFFFF}")
            i += 1
    d += 1
print(f"total descriptors: {d}")