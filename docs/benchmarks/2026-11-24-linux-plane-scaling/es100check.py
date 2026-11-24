"""Writes a page that compiles the video plane's GL ES shaders in WebGL.

Usage: es100check.py SHADERS.json OUT.html (es100check.sh runs it). The page
compiles the vertex shader and each fragment shader, links each program,
draws it once over a 5x3 picture with the uniforms the plane sets, and
prints, for WebGL 1 and WebGL 2, whether each compiled and linked, the info
logs and the GL error after the draw.
"""

import json
import sys

PAGE = """<!doctype html>
<meta charset="utf-8">
<title>Plane shaders in GLSL ES 1.00</title>
<pre id="out">running</pre>
<script>
const S = %s;
function run(version) {
  const gl = document.createElement('canvas').getContext(version);
  if (!gl) return {version, error: 'no context'};
  const result = {version, renderer: gl.getParameter(gl.VERSION) + ' / ' +
      gl.getParameter(gl.SHADING_LANGUAGE_VERSION), programs: {}};
  function compile(type, source) {
    const s = gl.createShader(type);
    gl.shaderSource(s, source);
    gl.compileShader(s);
    return {shader: s, ok: gl.getShaderParameter(s, gl.COMPILE_STATUS), log: gl.getShaderInfoLog(s)};
  }
  const vs = compile(gl.VERTEX_SHADER, S.vertex);
  result.vertex = {compiled: vs.ok, log: vs.log};
  const tex = gl.createTexture();
  gl.bindTexture(gl.TEXTURE_2D, tex);
  const px = new Uint8Array(5 * 3 * 4);
  for (let i = 0; i < px.length; ++i) px[i] = (i * 37) & 255;
  gl.texImage2D(gl.TEXTURE_2D, 0, gl.RGBA, 5, 3, 0, gl.RGBA, gl.UNSIGNED_BYTE, px);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
  gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
  gl.bindBuffer(gl.ARRAY_BUFFER, gl.createBuffer());
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 1, -1, -1, 1, 1, 1]), gl.STATIC_DRAW);
  for (const name of ['plain', 'area', 'upscale', 'sharpen']) {
    const fs = compile(gl.FRAGMENT_SHADER, S[name]);
    const entry = {compiled: fs.ok, log: fs.log};
    if (vs.ok && fs.ok) {
      const p = gl.createProgram();
      gl.attachShader(p, vs.shader);
      gl.attachShader(p, fs.shader);
      gl.bindAttribLocation(p, 0, 'position');
      gl.linkProgram(p);
      entry.linked = gl.getProgramParameter(p, gl.LINK_STATUS);
      entry.log += gl.getProgramInfoLog(p);
      if (entry.linked) {
        gl.useProgram(p);
        const u = n => gl.getUniformLocation(p, n);
        gl.uniform1i(u('picture'), 0);
        gl.uniform4f(u('bounds'), -1, -1, 1, 1);
        gl.uniform4f(u('window'), 0, 1, 1, 0);
        if (u('tone')) gl.uniformMatrix3fv(u('tone'), false, [1, 0, 0, 0, 1, 0, 0, 0, 1]);
        if (u('spacing')) gl.uniform2f(u('spacing'), 0.01, 0.01);
        if (u('bias')) gl.uniform1f(u('bias'), -1.585);
        if (u('size')) gl.uniform2f(u('size'), 5, 3);
        if (u('texel')) gl.uniform2f(u('texel'), 1 / 5, 1 / 3);
        if (u('limit')) gl.uniform2f(u('limit'), 0.9, 0.833);
        if (u('peak')) gl.uniform1f(u('peak'), -0.1);
        gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
        gl.enableVertexAttribArray(0);
        gl.drawArrays(gl.TRIANGLE_STRIP, 0, 4);
        entry.glError = gl.getError();
      }
    }
    result.programs[name] = entry;
  }
  return result;
}
document.getElementById('out').textContent =
    JSON.stringify([run('webgl'), run('webgl2')], null, 1);
</script>
"""


def main():
    with open(sys.argv[1]) as source:
        shaders = json.load(source)
    with open(sys.argv[2], "w") as out:
        out.write(PAGE % json.dumps(shaders))


if __name__ == "__main__":
    main()
