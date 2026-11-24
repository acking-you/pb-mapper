// Prints the video plane's shaders exactly as the Linux runner builds them
// (ui/linux/runner/desktop_video_gl.cc, included whole so that its private
// sources and ShaderHeader are in reach) for a GL ES context, GLSL ES 1.00,
// as a JSON object for es100check.py. Built by es100check.sh.
#include "desktop_video_gl.cc"

#include <cstdio>

namespace {
std::string Json(const std::string& text) {
  std::string out = "\"";
  for (const char c : text) {
    if (c == '"' || c == '\\') {
      out += '\\';
      out += c;
    } else if (c == '\n') {
      out += "\\n";
    } else {
      out += c;
    }
  }
  return out + "\"";
}
}  // namespace

int main() {
  using namespace pb_video;
  const std::string fragment = ShaderHeader(false, GL_FRAGMENT_SHADER);
  printf("{\"vertex\": %s,\n", Json(ShaderHeader(false, GL_VERTEX_SHADER) + kPlaneVertexShader).c_str());
  printf("\"plain\": %s,\n", Json(fragment + kPlaneFragmentShader).c_str());
  printf("\"area\": %s,\n", Json(fragment + kPlaneAreaShader).c_str());
  printf("\"upscale\": %s,\n", Json(fragment + kPlaneUpscaleShader).c_str());
  printf("\"sharpen\": %s}\n", Json(fragment + kPlaneSharpenShader).c_str());
  return 0;
}
