import AVFoundation
import CoreVideo

// Five 64x48 frames at 10 per second, each in quadrants -- red top left, green
// top right, blue bottom left, white bottom right -- in a track turned a
// quarter clockwise, as a phone held upright records one: shown upright, it is
// 48x64, red top right. H.264, BT.709 said in the stream, in an MP4.
let url = URL(fileURLWithPath: CommandLine.arguments[1])
try? FileManager.default.removeItem(at: url)
let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
    AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 64, AVVideoHeightKey: 48,
    AVVideoColorPropertiesKey: [AVVideoColorPrimariesKey: AVVideoColorPrimaries_ITU_R_709_2,
                                AVVideoTransferFunctionKey: AVVideoTransferFunction_ITU_R_709_2,
                                AVVideoYCbCrMatrixKey: AVVideoYCbCrMatrix_ITU_R_709_2],
    AVVideoCompressionPropertiesKey: [AVVideoProfileLevelKey: AVVideoProfileLevelH264BaselineAutoLevel,
                                      AVVideoAverageBitRateKey: 2_000_000],
])
input.transform = CGAffineTransform(rotationAngle: .pi / 2)
let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 64,
    kCVPixelBufferHeightKey as String: 48,
])
writer.add(input)
writer.startWriting()
writer.startSession(atSourceTime: .zero)
for i in 0..<5 {
    while !input.isReadyForMoreMediaData { usleep(1000) }
    var buffer: CVPixelBuffer?
    CVPixelBufferPoolCreatePixelBuffer(nil, adaptor.pixelBufferPool!, &buffer)
    let pixels = buffer!
    CVPixelBufferLockBaseAddress(pixels, [])
    let base = CVPixelBufferGetBaseAddress(pixels)!.assumingMemoryBound(to: UInt8.self)
    let stride = CVPixelBufferGetBytesPerRow(pixels)
    for y in 0..<48 { for x in 0..<64 {
        // BGRA.
        let (r, g, b): (UInt8, UInt8, UInt8) = y < 24 ? (x < 32 ? (255, 0, 0) : (0, 255, 0))
                                                      : (x < 32 ? (0, 0, 255) : (255, 255, 255))
        let p = base + y * stride + x * 4
        p[0] = b; p[1] = g; p[2] = r; p[3] = 255
    } }
    CVPixelBufferUnlockBaseAddress(pixels, [])
    adaptor.append(pixels, withPresentationTime: CMTime(value: CMTimeValue(i), timescale: 10))
}
input.markAsFinished()
let done = DispatchSemaphore(value: 0)
writer.finishWriting { done.signal() }
done.wait()
print(writer.status == .completed ? "ok" : "failed: \(String(describing: writer.error))")
