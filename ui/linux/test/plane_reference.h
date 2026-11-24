// CPU references of the video plane's filters (the plane shaders in
// ui/linux/runner/desktop_video_gl.cc), for the clip harness
// (desktop_video_test.cc) and the record in
// docs/benchmarks/2026-11-24-linux-plane-scaling: the shaders' arithmetic
// in double, on RGBA pictures whose rows run top first, sampled as GL
// samples a texture clamped to its edge. GL's own bilinear weights are
// coarser (8 bits on most GPUs), so a GPU's output differs from these by a
// level or two in 255, more where sharpening amplifies it.
//
// A picture `width` x `height` on screen is the source scaled to that size;
// each function gives the part of it from (x0, y0), `columns` x `rows`.
#ifndef UI_LINUX_TEST_PLANE_REFERENCE_H_
#define UI_LINUX_TEST_PLANE_REFERENCE_H_

#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cstdlib>
#include <vector>

namespace plane_reference {

// An RGBA picture, rows top first.
struct Image {
  int width = 0, height = 0;
  std::vector<uint8_t> rgba;

  Image() = default;
  Image(int w, int h) : width(w), height(h), rgba(static_cast<size_t>(w) * static_cast<size_t>(h) * 4, 0) {}
  // Channel `c` of pixel (x, y), clamped to the edge, in 0-1.
  double At(int x, int y, int c) const {
    x = std::min(std::max(x, 0), width - 1);
    y = std::min(std::max(y, 0), height - 1);
    return rgba[(static_cast<size_t>(y) * static_cast<size_t>(width) + static_cast<size_t>(x)) * 4 +
                static_cast<size_t>(c)] /
           255.0;
  }
  void Set(int x, int y, const double rgb[3]) {
    uint8_t* pixel = &rgba[(static_cast<size_t>(y) * static_cast<size_t>(width) + static_cast<size_t>(x)) * 4];
    for (int c = 0; c < 3; ++c) {
      pixel[c] = static_cast<uint8_t>(std::lround(std::min(std::max(rgb[c], 0.0), 1.0) * 255));
    }
    pixel[3] = 255;
  }
  // The part from (x0, y0), `columns` x `rows`, which lies inside.
  Image Crop(int x0, int y0, int columns, int rows) const {
    Image out(columns, rows);
    for (int y = 0; y < rows; ++y) {
      const auto from = rgba.begin() + (static_cast<ptrdiff_t>(y0 + y) * width + x0) * 4;
      std::copy(from, from + static_cast<ptrdiff_t>(columns) * 4,
                out.rgba.begin() + static_cast<ptrdiff_t>(y) * columns * 4);
    }
    return out;
  }
};

// Bilinear at (u, v) in texels from the top left, texel centres at k + 0.5.
inline double Bilinear(const Image& image, double u, double v, int c) {
  const double x = u - 0.5, y = v - 0.5;
  const int x0 = static_cast<int>(std::floor(x)), y0 = static_cast<int>(std::floor(y));
  const double fx = x - x0, fy = y - y0;
  return (image.At(x0, y0, c) * (1 - fx) + image.At(x0 + 1, y0, c) * fx) * (1 - fy) +
         (image.At(x0, y0 + 1, c) * (1 - fx) + image.At(x0 + 1, y0 + 1, c) * fx) * fy;
}

// The plane's bilinear draw.
inline Image Stretch(const Image& image, int width, int height, int x0, int y0, int columns, int rows) {
  Image out(columns, rows);
  for (int y = 0; y < rows; ++y) {
    for (int x = 0; x < columns; ++x) {
      double rgb[3];
      for (int c = 0; c < 3; ++c) {
        rgb[c] = Bilinear(image, (x0 + x + 0.5) * image.width / width, (y0 + y + 0.5) * image.height / height, c);
      }
      out.Set(x, y, rgb);
    }
  }
  return out;
}

// The area filter without mipmaps: nine bilinear taps over the span one
// pixel covers, averaged, sqrt((span^2 - 1) / 8) source pixels apart on
// each axis, the spread of the exact area average.
inline Image Area(const Image& image, int width, int height, int x0, int y0, int columns, int rows) {
  Image out(columns, rows);
  const double span_x = static_cast<double>(image.width) / width, span_y = static_cast<double>(image.height) / height;
  const double step_x = std::sqrt(std::max(span_x * span_x - 1, 0.0) / 8);
  const double step_y = std::sqrt(std::max(span_y * span_y - 1, 0.0) / 8);
  for (int y = 0; y < rows; ++y) {
    for (int x = 0; x < columns; ++x) {
      double rgb[3] = {0, 0, 0};
      for (int i = 0; i < 3; ++i) {
        for (int j = 0; j < 3; ++j) {
          const double u = (x0 + x + 0.5) * span_x + (i - 1) * step_x;
          const double v = (y0 + y + 0.5) * span_y + (j - 1) * step_y;
          for (int c = 0; c < 3; ++c) rgb[c] += Bilinear(image, u, v, c) / 9;
        }
      }
      out.Set(x, y, rgb);
    }
  }
  return out;
}

// The exact area average: each pixel the mean of the source over the span
// it covers, each source pixel weighted by its overlap.
inline Image Box(const Image& image, int width, int height, int x0, int y0, int columns, int rows) {
  Image out(columns, rows);
  const double span_x = static_cast<double>(image.width) / width, span_y = static_cast<double>(image.height) / height;
  for (int y = 0; y < rows; ++y) {
    const double top = (y0 + y) * span_y, bottom = top + span_y;
    for (int x = 0; x < columns; ++x) {
      const double left = (x0 + x) * span_x, right = left + span_x;
      double rgb[3] = {0, 0, 0}, total = 0;
      for (int sy = static_cast<int>(std::floor(top)); sy < static_cast<int>(std::ceil(bottom)); ++sy) {
        const double wy = std::min<double>(bottom, sy + 1) - std::max<double>(top, sy);
        for (int sx = static_cast<int>(std::floor(left)); sx < static_cast<int>(std::ceil(right)); ++sx) {
          const double w = wy * (std::min<double>(right, sx + 1) - std::max<double>(left, sx));
          for (int c = 0; c < 3; ++c) rgb[c] += image.At(sx, sy, c) * w;
          total += w;
        }
      }
      for (double& value : rgb) value /= total;
      out.Set(x, y, rgb);
    }
  }
  return out;
}

// Lanczos-3, separable, widened by the scale where it shrinks: the CPU
// reference a scaled picture is compared with. The whole picture.
inline Image Lanczos(const Image& image, int width, int height) {
  struct Tap {
    int index;
    double weight;
  };
  // One axis: `count` outputs from `source` inputs, each its taps.
  const auto taps = [](int source, int count) {
    const double pi = 3.14159265358979323846;
    const auto kernel = [pi](double x) {
      x = std::fabs(x);
      if (x < 1e-9) return 1.0;
      if (x >= 3) return 0.0;
      return 3 * std::sin(pi * x) * std::sin(pi * x / 3) / (pi * pi * x * x);
    };
    std::vector<std::vector<Tap>> all(static_cast<size_t>(count));
    const double scale = static_cast<double>(source) / count, stretch = std::max(1.0, scale);
    for (int o = 0; o < count; ++o) {
      const double centre = (o + 0.5) * scale;
      std::vector<Tap>& mine = all[static_cast<size_t>(o)];
      double total = 0;
      const int first = static_cast<int>(std::floor(centre - 3 * stretch));
      const int last = static_cast<int>(std::ceil(centre + 3 * stretch));
      for (int i = first; i <= last; ++i) {
        const double w = kernel((i + 0.5 - centre) / stretch);
        if (w == 0) continue;
        mine.push_back({std::min(std::max(i, 0), source - 1), w});
        total += w;
      }
      for (Tap& tap : mine) tap.weight /= total;
    }
    return all;
  };
  const std::vector<std::vector<Tap>> across = taps(image.width, width), down = taps(image.height, height);
  std::vector<double> rows(static_cast<size_t>(width) * static_cast<size_t>(image.height) * 3);
  const auto row_at = [width](int y, int x, int c) {
    return (static_cast<size_t>(y) * static_cast<size_t>(width) + static_cast<size_t>(x)) * 3 + static_cast<size_t>(c);
  };
  for (int y = 0; y < image.height; ++y) {
    for (int x = 0; x < width; ++x) {
      for (int c = 0; c < 3; ++c) {
        double sum = 0;
        for (const Tap& tap : across[static_cast<size_t>(x)]) sum += image.At(tap.index, y, c) * tap.weight;
        rows[row_at(y, x, c)] = sum;
      }
    }
  }
  Image out(width, height);
  for (int y = 0; y < height; ++y) {
    for (int x = 0; x < width; ++x) {
      double rgb[3] = {0, 0, 0};
      for (const Tap& tap : down[static_cast<size_t>(y)]) {
        for (int c = 0; c < 3; ++c) rgb[c] += rows[row_at(tap.index, x, c)] * tap.weight;
      }
      out.Set(x, y, rgb);
    }
  }
  return out;
}

// Catmull-Rom at (u, v) in texels, from nine bilinear taps as the shader
// takes them, clamped to the four texels nearest.
inline void CatmullRom(const Image& image, double u, double v, double rgb[3]) {
  const auto weights = [](double f, double w[4]) {
    w[0] = f * (-0.5 + f * (1.0 - 0.5 * f));
    w[1] = 1.0 + f * f * (-2.5 + 1.5 * f);
    w[2] = f * (0.5 + f * (2.0 - 1.5 * f));
    w[3] = f * f * (-0.5 + 0.5 * f);
  };
  const double t1x = std::floor(u - 0.5) + 0.5, t1y = std::floor(v - 0.5) + 0.5;
  double wx[4], wy[4];
  weights(u - t1x, wx);
  weights(v - t1y, wy);
  const double xs[3] = {t1x - 1, t1x + wx[2] / (wx[1] + wx[2]), t1x + 2};
  const double ys[3] = {t1y - 1, t1y + wy[2] / (wy[1] + wy[2]), t1y + 2};
  const double kx[3] = {wx[0], wx[1] + wx[2], wx[3]}, ky[3] = {wy[0], wy[1] + wy[2], wy[3]};
  const int nx = static_cast<int>(std::floor(t1x)), ny = static_cast<int>(std::floor(t1y));
  for (int c = 0; c < 3; ++c) {
    double sum = 0;
    for (int j = 0; j < 3; ++j) {
      for (int i = 0; i < 3; ++i) sum += Bilinear(image, xs[i], ys[j], c) * kx[i] * ky[j];
    }
    const double a = image.At(nx, ny, c), b = image.At(nx + 1, ny, c), d = image.At(nx, ny + 1, c),
                 e = image.At(nx + 1, ny + 1, c);
    const double low = std::min(std::min(a, b), std::min(d, e)), high = std::max(std::max(a, b), std::max(d, e));
    rgb[c] = std::min(std::max(sum, low), high);
  }
}

// The clamped Catmull-Rom enlargement, 8 bits a channel, as the plane's
// enlargement texture holds it.
inline Image Enlarge(const Image& image, int width, int height, int x0, int y0, int columns, int rows) {
  Image out(columns, rows);
  for (int y = 0; y < rows; ++y) {
    for (int x = 0; x < columns; ++x) {
      double rgb[3];
      CatmullRom(image, (x0 + x + 0.5) * image.width / width, (y0 + y + 0.5) * image.height / height, rgb);
      out.Set(x, y, rgb);
    }
  }
  return out;
}

inline double Luma(const Image& image, int x, int y) {
  return 0.2126 * image.At(x, y, 0) + 0.7152 * image.At(x, y, 1) + 0.0722 * image.At(x, y, 2);
}

// CAS on luma with `peak` at pixel (x, y), its neighbours clamped to the
// image; every channel moves by the luma's change.
inline void Sharpen(const Image& image, int x, int y, double peak, double rgb[3]) {
  const double e = Luma(image, x, y);
  const double a = Luma(image, x - 1, y - 1), b = Luma(image, x, y - 1), c = Luma(image, x + 1, y - 1);
  const double d = Luma(image, x - 1, y), f = Luma(image, x + 1, y);
  const double g = Luma(image, x - 1, y + 1), h = Luma(image, x, y + 1), i = Luma(image, x + 1, y + 1);
  double mn = std::min(std::min(std::min(d, e), std::min(f, b)), h);
  mn += std::min(mn, std::min(std::min(a, c), std::min(g, i)));
  double mx = std::max(std::max(std::max(d, e), std::max(f, b)), h);
  mx += std::max(mx, std::max(std::max(a, c), std::max(g, i)));
  const double amp = std::sqrt(std::min(std::max(std::min(mn, 2.0 - mx) / std::max(mx, 1e-5), 0.0), 1.0));
  const double w = amp * peak;
  const double l = (e + w * (b + d + f + h)) / (1.0 + 4.0 * w);
  for (int k = 0; k < 3; ++k) rgb[k] = std::min(std::max(image.At(x, y, k) + (l - e), 0.0), 1.0);
}

// The grey still's matrix: every channel BT.709's luma at half brightness.
inline Image Grey(const Image& image) {
  Image out(image.width, image.height);
  for (int y = 0; y < image.height; ++y) {
    for (int x = 0; x < image.width; ++x) {
      const double luma = 0.1063 * image.At(x, y, 0) + 0.3576 * image.At(x, y, 1) + 0.0361 * image.At(x, y, 2);
      const double rgb[3] = {luma, luma, luma};
      out.Set(x, y, rgb);
    }
  }
  return out;
}

// The plane's filters, as PlanPlane picks them; kBox stands for the
// mipmapped area filter, which approximates it.
enum class Filter { kBilinear, kArea, kBox, kSharpen, kUpscaleSharpen };

// What the plane draws of `picture` shown `width` x `height`, for the
// visible part from (x0, y0), `columns` x `rows`: kSharpen at the
// picture's size; kUpscaleSharpen enlarges the visible part and a pixel
// around it inside the picture, then sharpens with `peak`, its neighbours
// clamped to that region.
inline Image Expected(const Image& picture, Filter filter, int width, int height, int x0, int y0, int columns,
                      int rows, double peak, bool grey) {
  Image out;
  switch (filter) {
    case Filter::kBilinear:
      out = Stretch(picture, width, height, x0, y0, columns, rows);
      break;
    case Filter::kArea:
      out = Area(picture, width, height, x0, y0, columns, rows);
      break;
    case Filter::kBox:
      out = Box(picture, width, height, x0, y0, columns, rows);
      break;
    case Filter::kSharpen:
    case Filter::kUpscaleSharpen: {
      const bool enlarged = filter == Filter::kUpscaleSharpen;
      const int left = enlarged ? std::max(x0 - 1, 0) : 0, top = enlarged ? std::max(y0 - 1, 0) : 0;
      const int right = enlarged ? std::min(x0 + columns + 1, width) : width;
      const int bottom = enlarged ? std::min(y0 + rows + 1, height) : height;
      const Image region =
          enlarged ? Enlarge(picture, width, height, left, top, right - left, bottom - top) : picture;
      out = Image(columns, rows);
      for (int y = 0; y < rows; ++y) {
        for (int x = 0; x < columns; ++x) {
          double rgb[3];
          Sharpen(region, x0 + x - left, y0 + y - top, peak, rgb);
          out.Set(x, y, rgb);
        }
      }
      break;
    }
  }
  return grey ? Grey(out) : out;
}

// PSNR of the RGB of two pictures of one size, in dB; 99 when equal.
inline double Psnr(const Image& a, const Image& b) {
  double sum = 0;
  size_t count = 0;
  for (size_t i = 0; i < a.rgba.size() && i < b.rgba.size(); ++i) {
    if (i % 4 == 3) continue;
    const double d = static_cast<double>(a.rgba[i]) - b.rgba[i];
    sum += d * d;
    ++count;
  }
  if (count == 0 || sum == 0) return 99;
  return 10 * std::log10(255.0 * 255.0 / (sum / static_cast<double>(count)));
}

// The largest difference of a channel between two pictures of one size.
inline int MaxDifference(const Image& a, const Image& b) {
  int most = 0;
  for (size_t i = 0; i < a.rgba.size() && i < b.rgba.size(); ++i) {
    if (i % 4 != 3) most = std::max(most, std::abs(static_cast<int>(a.rgba[i]) - static_cast<int>(b.rgba[i])));
  }
  return most;
}

// The mean SSIM of two pictures' luma (BT.709 weights, 0-255), over 8x8
// windows a pixel apart, with the usual constants.
inline double Ssim(const Image& a, const Image& b) {
  const int size = 8;
  if (a.width != b.width || a.height != b.height || a.width < size || a.height < size) return 0;
  const double c1 = (0.01 * 255) * (0.01 * 255), c2 = (0.03 * 255) * (0.03 * 255);
  const size_t stride = static_cast<size_t>(a.width);
  std::vector<double> la(stride * static_cast<size_t>(a.height)), lb(la.size());
  for (int y = 0; y < a.height; ++y) {
    for (int x = 0; x < a.width; ++x) {
      la[static_cast<size_t>(y) * stride + static_cast<size_t>(x)] = Luma(a, x, y) * 255;
      lb[static_cast<size_t>(y) * stride + static_cast<size_t>(x)] = Luma(b, x, y) * 255;
    }
  }
  double total = 0;
  size_t windows = 0;
  const double n = size * size;
  for (int y = 0; y + size <= a.height; ++y) {
    for (int x = 0; x + size <= a.width; ++x) {
      double sa = 0, sb = 0, saa = 0, sbb = 0, sab = 0;
      for (int j = 0; j < size; ++j) {
        const size_t row = static_cast<size_t>(y + j) * stride + static_cast<size_t>(x);
        for (int i = 0; i < size; ++i) {
          const double va = la[row + static_cast<size_t>(i)], vb = lb[row + static_cast<size_t>(i)];
          sa += va;
          sb += vb;
          saa += va * va;
          sbb += vb * vb;
          sab += va * vb;
        }
      }
      const double ma = sa / n, mb = sb / n;
      const double va = (saa - n * ma * ma) / (n - 1), vb = (sbb - n * mb * mb) / (n - 1);
      const double cov = (sab - n * ma * mb) / (n - 1);
      total += ((2 * ma * mb + c1) * (2 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
      ++windows;
    }
  }
  return total / static_cast<double>(windows);
}

}  // namespace plane_reference

#endif  // UI_LINUX_TEST_PLANE_REFERENCE_H_
