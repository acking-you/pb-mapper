// The Windows HDR desktop as Desktop Duplication hands it out in FP16:
// linear scRGB, sRGB primaries, 1.0 = 80 nits, with SDR content at the
// display's SDR white level. This draws it into a BGRA8 render target as
// SDR: compiled by tonemap.rs, which documents it.
Texture2D<float4> desktop : register(t0);

cbuffer White : register(b0) {
    // 80 nits over the display's SDR white level, which brings SDR white
    // to 1.0.
    float gain;
};

// One clockwise triangle over the whole viewport, with no vertex buffer:
// (-1, 1), (3, 1) and (-1, -3).
float4 vertex(uint id : SV_VertexID) : SV_Position {
    float2 corner = float2((id << 1) & 2, id & 2);
    return float4(corner * float2(2, -2) + float2(-1, 1), 0, 1);
}

float4 pixel(float4 position : SV_Position) : SV_Target {
    // SDR white to 1.0; a colour outside the sRGB gamut, whose scRGB has a
    // negative channel, clipped to it.
    float3 rgb = max(desktop.Load(int3(position.xy, 0)).rgb * gain, 0);
    // Brighter than SDR white (HDR video, a game): brought down to it by
    // one gain on all three channels, which keeps the colour's hue. SDR
    // content, at most SDR white, is left as it is.
    float peak = max(rgb.r, max(rgb.g, rgb.b));
    rgb /= max(peak, 1);
    // The sRGB transfer function, which the BGRA8 picture carries.
    float3 curve = 1.055 * pow(rgb, 1.0 / 2.4) - 0.055;
    return float4(lerp(12.92 * rgb, curve, step(0.0031308, rgb)), 1);
}
