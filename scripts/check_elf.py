"""Prints the shared libraries a Linux binary links against and the newest glibc symbol version it needs.
The Steam Deck build must only need libc/libm/libdl/libpthread/libgcc_s and glibc <= 2.28.
Usage: python -I scripts/check_elf.py target/x86_64-unknown-linux-gnu/release/BonfireLink"""
import re
import struct
import sys

ALLOWED = {"libc.so.6", "libm.so.6", "libdl.so.2", "libpthread.so.0", "libgcc_s.so.1", "ld-linux-x86-64.so.2", "librt.so.1"}


def needed(data):
    shoff, = struct.unpack_from("<Q", data, 0x28)
    shentsize, shnum = struct.unpack_from("<HH", data, 0x3A)
    sections = [struct.unpack_from("<IIQQQQIIQQ", data, shoff + i * shentsize) for i in range(shnum)]
    out = []
    for sh in sections:
        if sh[1] != 6:  # SHT_DYNAMIC
            continue
        strtab = sections[sh[6]]
        off, size = sh[4], sh[5]
        for i in range(0, size, 16):
            tag, val = struct.unpack_from("<qQ", data, off + i)
            if tag == 0:
                break
            if tag == 1:  # DT_NEEDED
                start = strtab[4] + val
                out.append(data[start:data.index(b"\0", start)].decode())
    return out


def main():
    data = open(sys.argv[1], "rb").read()
    libs = needed(data)
    versions = sorted({tuple(map(int, v.split("."))) for v in re.findall(rb"GLIBC_(\d+\.\d+(?:\.\d+)?)", data) for v in [v.decode()]})
    newest = ".".join(map(str, versions[-1])) if versions else "none"
    print("NEEDED:", ", ".join(libs))
    print("newest GLIBC symbol version:", newest)
    bad = [l for l in libs if l not in ALLOWED]
    if bad or (versions and versions[-1] > (2, 28)):
        print("NOT OK for SteamOS:", bad or newest)
        sys.exit(1)
    print("OK for every SteamOS 3 release")


main()
