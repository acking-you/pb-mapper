"""Builds H.264 SPS test vectors bit by bit, independently of the Rust writer,
in the shapes a hardware encoder may write and VideoToolbox does not: timing
and an HRD in the VUI, scaling matrices, picture order type 1, field coding,
4:4:4 with a separate colour plane, and a restriction that allows reordering."""


class W:
    def __init__(self):
        self.bits = []

    def u(self, n, v):
        self.bits += [(v >> (n - 1 - i)) & 1 for i in range(n)]

    def ue(self, v):
        code = v + 1
        n = code.bit_length()
        self.bits += [0] * (n - 1)
        self.u(n, code)

    def se(self, v):
        self.ue(2 * v - 1 if v > 0 else -2 * v)

    def rbsp(self):
        bits = self.bits + [1]
        while len(bits) % 8:
            bits.append(0)
        return bytes(int(''.join(map(str, bits[i:i + 8])), 2) for i in range(0, len(bits), 8))


def escape(rbsp):
    out, zeros = bytearray(), 0
    for b in rbsp:
        if zeros >= 2 and b <= 3:
            out.append(3)
            zeros = 0
        zeros = zeros + 1 if b == 0 else 0
        out.append(b)
    return bytes(out)


def hrd_params(w):
    w.ue(0)  # cpb_cnt_minus1
    w.u(4, 0)  # bit_rate_scale
    w.u(4, 3)  # cpb_size_scale
    w.ue(15624)  # bit_rate_value_minus1
    w.ue(31249)  # cpb_size_value_minus1
    w.u(1, 1)  # cbr_flag
    w.u(5, 23)
    w.u(5, 23)
    w.u(5, 23)
    w.u(5, 24)


def timing_hrd_high_1080p():
    """High, 4:2:0, POC 2, VUI: video signal, timing 1/120 (whose 32-bit
    num_units_in_tick of 1 needs emulation prevention), a NAL HRD, a
    restriction allowing 2 reordered frames."""
    w = W()
    w.u(8, 100); w.u(8, 0); w.u(8, 42); w.ue(0)
    w.ue(1); w.ue(0); w.ue(0); w.u(1, 0); w.u(1, 0)  # chroma 1, 8-bit, no bypass, no scaling
    w.ue(0)  # log2_max_frame_num_minus4
    w.ue(2)  # POC type 2
    w.ue(1)  # refs
    w.u(1, 0)
    w.ue(119); w.ue(67)
    w.u(1, 1); w.u(1, 1)  # frame_mbs_only, direct_8x8
    w.u(1, 1); w.ue(0); w.ue(0); w.ue(0); w.ue(4)  # crop bottom 4
    w.u(1, 1)  # VUI
    w.u(1, 1); w.u(8, 1)  # aspect 1:1
    w.u(1, 0)  # overscan
    w.u(1, 1); w.u(3, 5); w.u(1, 0); w.u(1, 1); w.u(8, 1); w.u(8, 1); w.u(8, 1)
    w.u(1, 0)  # chroma loc
    w.u(1, 1); w.u(32, 1); w.u(32, 120); w.u(1, 0)  # timing
    w.u(1, 1); hrd_params(w)  # NAL HRD
    w.u(1, 0)  # VCL HRD
    w.u(1, 0)  # low_delay_hrd_flag
    w.u(1, 0)  # pic_struct
    w.u(1, 1)  # restriction
    w.u(1, 1); w.ue(0); w.ue(0); w.ue(16); w.ue(16); w.ue(2); w.ue(3)
    return w


def scaling_cycle_fields():
    """High 4:2:0 with scaling matrices (lists 0, 1 and 6, one of them
    switched to its default), POC type 1, field coding and no VUI."""
    w = W()
    w.u(8, 100); w.u(8, 0x0c); w.u(8, 40); w.ue(3)
    w.ue(1); w.ue(0); w.ue(0); w.u(1, 0)
    w.u(1, 1)  # seq_scaling_matrix_present_flag
    for i in range(8):
        if i == 0:
            w.u(1, 1)
            for _ in range(16):
                w.se(1)  # 9, 10, 11 ...
        elif i == 1:
            w.u(1, 1)
            w.se(-8)  # next 0 at j == 0: the default list
        elif i == 6:
            w.u(1, 1)
            for j in range(64):
                if j == 10:
                    w.se(-30)  # back to 8 ... then 0: the rest repeat
                    break
                w.se(2)
            # 8 + 2*10 = 28, 28 - 30 + 256 = 254 % 256: not 0, continue
            # until the list ends
            for _ in range(64 - 11):
                w.se(0)
        else:
            w.u(1, 0)
    w.ue(2)  # log2_max_frame_num_minus4
    w.ue(1)  # POC type 1
    w.u(1, 0); w.se(-1); w.se(1); w.ue(3); w.se(2); w.se(-2); w.se(4)
    w.ue(2)  # refs
    w.u(1, 0)
    w.ue(44); w.ue(17)
    w.u(1, 0); w.u(1, 1)  # field coding, MBAFF
    w.u(1, 1)
    w.u(1, 0)  # no crop
    w.u(1, 0)  # no VUI
    return w


def separate_planes_444():
    """High 4:4:4 Predictive with a separate colour plane and 10-bit luma, POC
    type 0, a VUI with only pic_struct and no restriction."""
    w = W()
    w.u(8, 244); w.u(8, 0); w.u(8, 51); w.ue(0)
    w.ue(3); w.u(1, 1); w.ue(2); w.ue(2); w.u(1, 1); w.u(1, 0)
    w.ue(0); w.ue(0); w.ue(4)
    w.ue(4); w.u(1, 1)
    w.ue(79); w.ue(44)
    w.u(1, 1); w.u(1, 0)
    w.u(1, 0)
    w.u(1, 1)  # VUI
    w.u(1, 0); w.u(1, 1); w.u(1, 1); w.u(1, 0); w.u(1, 1); w.ue(1); w.ue(1)
    w.u(1, 0); w.u(1, 0); w.u(1, 0); w.u(1, 1)  # no timing, no HRDs, pic_struct
    w.u(1, 0)  # no restriction
    return w


for name, build, header in [
    ('timing_hrd_high_1080p', timing_hrd_high_1080p, 0x67),
    ('scaling_cycle_fields', scaling_cycle_fields, 0x67),
    ('separate_planes_444', separate_planes_444, 0x27),
]:
    nal = bytes([header]) + escape(build().rbsp())
    print(name, nal.hex())
