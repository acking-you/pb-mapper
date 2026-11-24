"""Expected rewrites, built from the bit positions sps_dump.py finds: the
bits up to the VUI flag or the restriction flag, then the restriction with
max_num_reorder_frames 0 and max_dec_frame_buffering the reference frames."""
import os
import sys

here = os.path.dirname(os.path.abspath(__file__))
exec(open(os.path.join(here, 'sps_dump.py')).read().split('for line in sys.stdin')[0])
exec(open(os.path.join(here, 'sps_build.py')).read().split('\nfor name, build')[0])


def bits_of(data):
    return [(b >> (7 - i)) & 1 for b in data for i in range(8)]


def ue_bits(v):
    w = W()
    w.ue(v)
    return w.bits


def rewrite(hexstr):
    nal = bytes.fromhex(hexstr)
    o = dump(hexstr)
    rb = bits_of(rbsp(nal[1:]))
    refs = o['refs']

    def restr(mv, bd, md, h, v):
        return [mv] + ue_bits(bd) + ue_bits(md) + ue_bits(h) + ue_bits(v) + ue_bits(0) + ue_bits(refs)

    if not o['vui']:
        out = rb[:o['trailing_at'] - 1] + [1] + [0] * 8 + [1] + restr(1, 0, 0, 15, 15)
    elif 'restriction' not in o:
        out = rb[:o['restriction_at']] + [1] + restr(1, 0, 0, 15, 15)
    else:
        r = o['restriction']
        out = rb[:o['restriction_at']] + [1] + restr(r['mv'], r['bytes_denom'], r['bits_denom'], r['mv_h'], r['mv_v'])
    w = W()
    w.bits = out
    return (bytes([nal[0]]) + escape(w.rbsp())).hex()


samples = []
for line in open(os.path.join(here, 'vt_sps_samples.txt')):
    name = line.split()[0]
    sps = line.split('sets=')[1].split(',')[0]
    samples.append((name, sps))
for line in open(os.path.join(here, 'built.txt')):
    name, sps = line.split()
    if True:
        samples.append((name, sps))
for name, sps in samples:
    print(name, sps, '->', rewrite(sps))
