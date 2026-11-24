// Does Core Media's H.264 format description reject a damaged SPS? Probes
// whether the rewrite test's Core Media check has teeth.
import CoreMedia
import Foundation

func bytes(_ hex: String) -> [UInt8] {
  var out: [UInt8] = []
  var index = hex.startIndex
  while index < hex.endIndex {
    let next = hex.index(index, offsetBy: 2)
    out.append(UInt8(hex[index..<next], radix: 16)!)
    index = next
  }
  return out
}

func status(_ sps: [UInt8], _ pps: [UInt8]) -> (OSStatus, Int32, Int32) {
  var format: CMFormatDescription?
  let result = sps.withUnsafeBufferPointer { s in
    pps.withUnsafeBufferPointer { p in
      let pointers = [s.baseAddress!, p.baseAddress!]
      let sizes = [sps.count, pps.count]
      return CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: nil, parameterSetCount: 2, parameterSetPointers: pointers, parameterSetSizes: sizes,
        nalUnitHeaderLength: 4, formatDescriptionOut: &format)
    }
  }
  let dims = format.map { CMVideoFormatDescriptionGetDimensions($0) }
  return (result, dims?.width ?? 0, dims?.height ?? 0)
}

let pps = bytes("28ee3cb0")
let good = bytes("27640020ac56805005ba01e1008540")
print("rewritten", status(good, pps))
print("truncated by 3", status(Array(good.dropLast(3)), pps))
print("truncated by 1", status(Array(good.dropLast(1)), pps))
var vuiOn = bytes("27640020ac56805005b9")
vuiOn[vuiOn.count - 1] = 0xbb  // VUI flag on, and nothing after it
print("vui flag without a vui", status(vuiOn, pps))
print("no sps body", status([0x27, 0x64], pps))
