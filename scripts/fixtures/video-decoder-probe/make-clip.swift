import AVFoundation
import CoreVideo

// Ten 64x48 frames at 10 per second, each one flat colour: frame i is
// (i * 25, 255 - i * 25, 128). H.264, in an MP4.
let url = URL(fileURLWithPath: CommandLine.arguments[1])
try? FileManager.default.removeItem(at: url)
let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
    AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: 64, AVVideoHeightKey: 48,
    // Said in the stream, so a decoder reads the colour as it was written.
    AVVideoColorPropertiesKey: [AVVideoColorPrimariesKey: AVVideoColorPrimaries_ITU_R_709_2,
                                AVVideoTransferFunctionKey: AVVideoTransferFunction_ITU_R_709_2,
                                AVVideoYCbCrMatrixKey: AVVideoYCbCrMatrix_ITU_R_709_2],
    AVVideoCompressionPropertiesKey: [AVVideoProfileLevelKey: AVVideoProfileLevelH264BaselineAutoLevel,
                                      AVVideoAverageBitRateKey: 2_000_000, AVVideoMaxKeyFrameIntervalKey: 5],
])
let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA, kCVPixelBufferWidthKey as String: 64,
    kCVPixelBufferHeightKey as String: 48,
])
writer.add(input)
writer.startWriting()
writer.startSession(atSourceTime: .zero)
for i in 0..<10 {
    while !input.isReadyForMoreMediaData { usleep(1000) }
    var buffer: CVPixelBuffer?
    CVPixelBufferPoolCreatePixelBuffer(nil, adaptor.pixelBufferPool!, &buffer)
    let pixels = buffer!
    CVPixelBufferLockBaseAddress(pixels, [])
    let base = CVPixelBufferGetBaseAddress(pixels)!.assumingMemoryBound(to: UInt8.self)
    let stride = CVPixelBufferGetBytesPerRow(pixels)
    for y in 0..<48 { for x in 0..<64 {
        let p = base + y * stride + x * 4
        p[0] = 128; p[1] = UInt8(255 - i * 25); p[2] = UInt8(i * 25); p[3] = 255
    } }
    CVPixelBufferUnlockBaseAddress(pixels, [])
    adaptor.append(pixels, withPresentationTime: CMTime(value: CMTimeValue(i), timescale: 10))
}
input.markAsFinished()
let done = DispatchSemaphore(value: 0)
writer.finishWriting { done.signal() }
done.wait()
print(writer.status == .completed ? "ok" : "failed: \(String(describing: writer.error))")
