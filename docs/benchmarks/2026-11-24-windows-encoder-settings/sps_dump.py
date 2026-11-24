"""Independent H.264 SPS dump (cross-check for the Rust parser's tests)."""
import sys


def rbsp(nal):
    out, zeros = [], 0
    for b in nal:
        if zeros >= 2 and b == 3:
            zeros = 0
            continue
        out.append(b)
        zeros = zeros + 1 if b == 0 else 0
    return out


class Bits:
    def __init__(self, data):
        self.d, self.p = data, 0

    def u(self, n):
        v = 0
        for _ in range(n):
            v = (v << 1) | ((self.d[self.p // 8] >> (7 - self.p % 8)) & 1)
            self.p += 1
        return v

    def ue(self):
        z = 0
        while self.u(1) == 0:
            z += 1
        return (1 << z) - 1 + (self.u(z) if z else 0)

    def se(self):
        k = self.ue()
        return (k + 1) // 2 if k % 2 else -(k // 2)


def hrd(b, out, name):
    cnt = b.ue()
    b.u(4); b.u(4)
    for _ in range(cnt + 1):
        b.ue(); b.ue(); b.u(1)
    b.u(5); b.u(5); b.u(5); b.u(5)
    out[name] = cnt


def dump(hexstr):
    nal = bytes.fromhex(hexstr)
    b = Bits(rbsp(nal[1:]))
    o = {}
    o['profile'] = b.u(8); o['flags'] = b.u(8); o['level'] = b.u(8); o['id'] = b.ue()
    if o['profile'] in (100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135):
        o['chroma'] = b.ue()
        if o['chroma'] == 3:
            b.u(1)
        o['bd_luma'] = b.ue(); o['bd_chroma'] = b.ue(); b.u(1)
        o['scaling'] = b.u(1)
        if o['scaling']:
            lists = []
            for i in range(12 if o['chroma'] == 3 else 8):
                if b.u(1):
                    size, last, nxt, values = (16 if i < 6 else 64), 8, 8, []
                    for j in range(size):
                        if nxt != 0:
                            nxt = (last + b.se() + 256) % 256
                        values.append(last if nxt == 0 else nxt)
                        last = values[-1]
                    lists.append((i, values[:3], values[-1]))
            o['lists'] = lists
    o['log2_frame_num'] = b.ue()
    o['poc'] = b.ue()
    if o['poc'] == 0:
        o['log2_poc_lsb'] = b.ue()
    elif o['poc'] == 1:
        o['poc1'] = (b.u(1), b.se(), b.se(), [b.se() for _ in range(b.ue())])
    o['refs'] = b.ue(); o['gaps'] = b.u(1)
    o['w_mbs'] = b.ue() + 1; o['h_units'] = b.ue() + 1
    o['frame_mbs_only'] = b.u(1)
    if not o['frame_mbs_only']:
        b.u(1)
    o['direct8x8'] = b.u(1)
    if b.u(1):
        o['crop'] = [b.ue(), b.ue(), b.ue(), b.ue()]
    o['vui'] = b.u(1)
    if o['vui']:
        o['vui_at'] = b.p
        if b.u(1):
            idc = b.u(8); o['aspect'] = idc
            if idc == 255:
                b.u(16); b.u(16)
        if b.u(1):
            b.u(1)
        if b.u(1):
            o['video_format'] = b.u(3); o['full_range'] = b.u(1)
            if b.u(1):
                o['colour'] = (b.u(8), b.u(8), b.u(8))
        if b.u(1):
            b.ue(); b.ue()
        if b.u(1):
            o['timing'] = (b.u(32), b.u(32), b.u(1))
        nal_hrd = b.u(1)
        if nal_hrd:
            hrd(b, o, 'nal_hrd')
        vcl_hrd = b.u(1)
        if vcl_hrd:
            hrd(b, o, 'vcl_hrd')
        if nal_hrd or vcl_hrd:
            b.u(1)
        o['pic_struct'] = b.u(1)
        o['restriction_at'] = b.p
        if b.u(1):
            o['restriction'] = dict(mv=b.u(1), bytes_denom=b.ue(), bits_denom=b.ue(), mv_h=b.ue(), mv_v=b.ue(),
                                    reorder=b.ue(), buffering=b.ue())
    o['trailing_at'] = b.p
    o['rbsp_bits'] = len(b.d) * 8
    return o


for line in sys.stdin:
    name, _, rest = line.partition(' ')
    sets = rest.split('sets=')[1].strip().split(',')
    print(name, dump(sets[0]))
