"""Turn the raw RGBA icons from `examples/iconset.rs` into a macOS .iconset.

Standard library only (zlib), so the CI runner needs nothing installed.
usage: rgba_to_iconset.py <rgba_dir> <out.iconset>
"""
import os
import struct
import sys
import zlib


def png(n: int, rgba: bytes) -> bytes:
    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xFFFFFFFF)

    rows = b''.join(b'\x00' + rgba[y * n * 4:(y + 1) * n * 4] for y in range(n))
    ihdr = struct.pack('>IIBBBBB', n, n, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', ihdr) + chunk(b'IDAT', zlib.compress(rows, 9)) + chunk(b'IEND', b'')


# iconset name -> pixel size
NAMES = {
    'icon_16x16.png': 16, 'icon_16x16@2x.png': 32,
    'icon_32x32.png': 32, 'icon_32x32@2x.png': 64,
    'icon_128x128.png': 128, 'icon_128x128@2x.png': 256,
    'icon_256x256.png': 256, 'icon_256x256@2x.png': 512,
    'icon_512x512.png': 512, 'icon_512x512@2x.png': 1024,
}

src, dst = sys.argv[1], sys.argv[2]
os.makedirs(dst, exist_ok=True)
for name, n in NAMES.items():
    with open(os.path.join(src, f'icon_{n}.rgba'), 'rb') as f:
        data = f.read()
    assert len(data) == n * n * 4, (name, len(data))
    with open(os.path.join(dst, name), 'wb') as f:
        f.write(png(n, data))
print(f'wrote {len(NAMES)} icons to {dst}')
