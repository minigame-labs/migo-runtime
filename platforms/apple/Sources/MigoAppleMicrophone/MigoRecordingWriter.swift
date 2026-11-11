import AVFoundation
import Foundation

/// One recording being written: the microphone's buffers, as they come, turned into the samples content asked
/// for -- its rate, its channels, 16 bits -- and those into the file and, when content asked for frames, the
/// bytes the frames are cut from. It counts what it recorded in samples, so a recording ends on the sample its
/// duration ends on, whatever sizes the microphone hands over. One queue's: not thread-safe.
///
/// What it writes, by format:
///   PCM  the samples, raw; frames are the same bytes.
///   wav  the samples behind a WAV header; frames are the samples.
///   aac  AAC: in an MPEG-4 file (.m4a) when content asked for no frames, and as an ADTS stream (.aac) when it
///        did -- a stream is what can be handed over as it is recorded, and frames are its bytes. The same two
///        files Android's recorder writes. The bit rate is the encoder's nearest to the one asked for.
final class MigoRecordingWriter {
    /// What one buffer of the microphone's came to.
    struct Step {
        /// Whole frames to hand content now.
        var frames: [Data] = []
        /// The recording holds its whole duration: it is to be finished.
        var full = false
    }

    /// The finished recording.
    struct Ending {
        let url: URL
        let durationMs: Int
        let fileSize: Int
        /// The frames still to hand content, the last of them the recording's last; empty when it asked for none.
        let frames: [Data]
    }

    let url: URL
    private let options: MigoRecordingOptions
    /// The samples the file is made from: 16-bit, interleaved, at the rate and channels asked for.
    private let samples: AVAudioFormat
    private var resampler: AVAudioConverter
    private var resampled: AVAudioPCMBuffer?
    private let sink: MigoRecordingSink
    private var recorded: Int64 = 0
    /// Bytes not yet a whole frame.
    private var pending = Data()
    /// Whole frames not yet handed over.
    private var ready: [Data] = []

    /// A recording of `options` from a microphone that hands over `input`, into a new file in `directory`.
    init(options: MigoRecordingOptions, input: AVAudioFormat, directory: URL) throws {
        guard
            let samples = AVAudioFormat(
                commonFormat: .pcmFormatInt16, sampleRate: Double(options.sampleRate),
                channels: AVAudioChannelCount(options.channels), interleaved: true)
        else {
            throw MigoRecordingError("invalid audio parameters")
        }
        self.options = options
        self.samples = samples
        resampler = try Self.resampler(from: input, to: samples)
        let name = "recording-\(UUID().uuidString)"
        switch options.format {
        case .pcm:
            url = directory.appendingPathComponent("\(name).pcm")
            sink = try MigoSampleSink(url: url, samples: samples, wav: false)
        case .wav:
            url = directory.appendingPathComponent("\(name).wav")
            sink = try MigoSampleSink(url: url, samples: samples, wav: true)
        case .aac where options.frameBytes > 0:
            url = directory.appendingPathComponent("\(name).aac")
            sink = try MigoAdtsSink(url: url, samples: samples, bitRate: options.bitRate)
        case .aac:
            url = directory.appendingPathComponent("\(name).m4a")
            sink = try MigoMpeg4Sink(url: url, samples: samples, bitRate: options.bitRate)
        }
    }

    /// The microphone changed -- another route, another rate -- or is listened to again after a pause: what it
    /// hands over from now is `input`. What the resampler still held of the sound before is written first.
    func retarget(input: AVAudioFormat) throws {
        let next = try Self.resampler(from: input, to: samples)
        try take(nil)
        resampler = next
        resampled = nil
    }

    /// One buffer of the microphone's, as far as the recording's duration has room for it.
    func append(_ buffer: AVAudioPCMBuffer) throws -> Step {
        try take(buffer)
        defer { ready = [] }
        return Step(frames: ready, full: recorded >= options.frameLimit)
    }

    /// The recording ends: what the converters still held is written, and the file closed.
    func finish() throws -> Ending {
        try take(nil)
        if let rest = try sink.finish() { cut(rest) }
        if options.frameBytes > 0 {
            // The last frame is what is left, which may be nothing: it is the one that says the recording ended.
            ready.append(pending)
            pending = Data()
        }
        defer { ready = [] }
        let size = (try FileManager.default.attributesOfItem(atPath: url.path)[.size] as? NSNumber)?.intValue ?? 0
        // To the nearest millisecond: a duration that ends between two samples is still that duration.
        let rate = Int64(options.sampleRate)
        return Ending(url: url, durationMs: Int((recorded * 1000 + rate / 2) / rate), fileSize: size, frames: ready)
    }

    /// The recording is not wanted: its file goes.
    func discard() {
        _ = try? sink.finish()
        try? FileManager.default.removeItem(at: url)
    }

    /// `buffer` resampled and written, or with `nil` what the resampler still held.
    private func take(_ buffer: AVAudioPCMBuffer?) throws {
        var input = buffer
        let ratio = samples.sampleRate / resampler.inputFormat.sampleRate
        let capacity = AVAudioFrameCount((Double(buffer?.frameLength ?? 0) * ratio).rounded(.up)) + 256
        if resampled == nil || resampled!.frameCapacity < capacity {
            resampled = AVAudioPCMBuffer(pcmFormat: samples, frameCapacity: capacity)
        }
        guard let output = resampled else { throw MigoRecordingError("out of memory") }
        while true {
            output.frameLength = 0
            var failure: NSError?
            let status = resampler.convert(to: output, error: &failure) { _, state in
                if let next = input {
                    input = nil
                    state.pointee = .haveData
                    return next
                }
                state.pointee = buffer == nil ? .endOfStream : .noDataNow
                return nil
            }
            if status == .error { throw failure ?? MigoRecordingError("the microphone's audio could not be read") }
            let room = options.frameLimit - recorded
            if output.frameLength > 0, room > 0 {
                if Int64(output.frameLength) > room { output.frameLength = AVAudioFrameCount(room) }
                recorded += Int64(output.frameLength)
                if let bytes = try sink.write(output) { cut(bytes) }
            }
            // More waits only when this pass filled the buffer.
            guard status == .haveData, output.frameLength == output.frameCapacity else { return }
        }
    }

    /// `bytes` joined to what was left, and every whole frame of it cut off.
    private func cut(_ bytes: Data) {
        let size = options.frameBytes
        guard size > 0 else { return }
        pending.append(bytes)
        var offset = 0
        while pending.count - offset >= size {
            ready.append(pending.subdata(in: offset..<offset + size))
            offset += size
        }
        if offset > 0 { pending = pending.subdata(in: offset..<pending.count) }
    }

    private static func resampler(from input: AVAudioFormat, to samples: AVAudioFormat) throws -> AVAudioConverter {
        guard input.sampleRate > 0, input.channelCount > 0, let converter = AVAudioConverter(from: input, to: samples)
        else {
            throw MigoRecordingError("there is no microphone to record from")
        }
        return converter
    }
}

/// Where a recording's samples go.
protocol MigoRecordingSink: AnyObject {
    /// Samples in: the bytes they became, for content's frames -- `nil` from a file that hands none over.
    func write(_ samples: AVAudioPCMBuffer) throws -> Data?
    /// The file finished and closed: the bytes it still held, likewise. Harmless twice.
    func finish() throws -> Data?
}

/// The samples themselves, raw or behind a WAV header.
final class MigoSampleSink: MigoRecordingSink {
    private var file: FileHandle?
    private let samples: AVAudioFormat
    private let wav: Bool
    private var written = 0

    init(url: URL, samples: AVAudioFormat, wav: Bool) throws {
        guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
            throw MigoRecordingError("there is no file to record into")
        }
        file = try FileHandle(forWritingTo: url)
        self.samples = samples
        self.wav = wav
        if wav { try file?.write(contentsOf: Self.header(samples, dataBytes: 0)) }
    }

    func write(_ samples: AVAudioPCMBuffer) throws -> Data? {
        guard let file, let channel = samples.int16ChannelData else { return nil }
        // Interleaved: the one buffer holds every channel's samples.
        let bytes = Data(
            bytes: channel[0],
            count: Int(samples.frameLength) * Int(samples.format.channelCount) * MemoryLayout<Int16>.size)
        try file.write(contentsOf: bytes)
        written += bytes.count
        return bytes
    }

    func finish() throws -> Data? {
        guard let file else { return nil }
        self.file = nil
        if wav {
            try file.seek(toOffset: 0)
            try file.write(contentsOf: Self.header(samples, dataBytes: written))
        }
        try file.close()
        return nil
    }

    /// A canonical WAV header for `dataBytes` of 16-bit samples.
    static func header(_ samples: AVAudioFormat, dataBytes: Int) -> Data {
        let rate = Int(samples.sampleRate)
        let channels = Int(samples.channelCount)
        var header = Data(capacity: 44)
        func u32(_ value: Int) { withUnsafeBytes(of: UInt32(value).littleEndian) { header.append(contentsOf: $0) } }
        func u16(_ value: Int) { withUnsafeBytes(of: UInt16(value).littleEndian) { header.append(contentsOf: $0) } }
        header.append(contentsOf: Array("RIFF".utf8))
        u32(36 + dataBytes)
        header.append(contentsOf: Array("WAVEfmt ".utf8))
        u32(16)
        u16(1)
        u16(channels)
        u32(rate)
        u32(rate * channels * 2)
        u16(channels * 2)
        u16(16)
        header.append(contentsOf: Array("data".utf8))
        u32(dataBytes)
        return header
    }
}

/// AAC, the system's encoder's. `nil` when it encodes no AAC at this rate and these channels.
private func aacFormat(for samples: AVAudioFormat) -> AVAudioFormat? {
    AVAudioFormat(settings: [
        AVFormatIDKey: kAudioFormatMPEG4AAC,
        AVSampleRateKey: samples.sampleRate,
        AVNumberOfChannelsKey: samples.channelCount,
    ])
}

/// The bit rate `encoder` takes that is nearest the one asked for: each sample rate has its own range, and the
/// reference's ranges are another encoder's.
private func nearestBitRate(_ asked: Int, of encoder: AVAudioConverter) -> Int {
    let rates = (encoder.applicableEncodeBitRates ?? []).map(\.intValue)
    return rates.min { abs($0 - asked) < abs($1 - asked) } ?? asked
}

/// AAC as an ADTS stream: each packet behind its own seven-byte header, so any run of whole packets plays.
final class MigoAdtsSink: MigoRecordingSink {
    /// MPEG-4's sampling frequency indices.
    private static let rates = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350]

    private var file: FileHandle?
    private let encoder: AVAudioConverter
    private let packets: AVAudioCompressedBuffer
    private let rateIndex: Int
    private let channels: Int

    init(url: URL, samples: AVAudioFormat, bitRate: Int) throws {
        guard let aac = aacFormat(for: samples), let encoder = AVAudioConverter(from: samples, to: aac),
            let rateIndex = Self.rates.firstIndex(of: Int(samples.sampleRate))
        else {
            throw MigoRecordingError("invalid audio parameters")
        }
        encoder.bitRate = nearestBitRate(bitRate, of: encoder)
        self.encoder = encoder
        self.rateIndex = rateIndex
        channels = Int(samples.channelCount)
        packets = AVAudioCompressedBuffer(
            format: aac, packetCapacity: 16, maximumPacketSize: encoder.maximumOutputPacketSize)
        guard FileManager.default.createFile(atPath: url.path, contents: nil) else {
            throw MigoRecordingError("there is no file to record into")
        }
        file = try FileHandle(forWritingTo: url)
    }

    func write(_ samples: AVAudioPCMBuffer) throws -> Data? {
        try encode(samples)
    }

    func finish() throws -> Data? {
        guard let file else { return nil }
        let rest = try encode(nil)
        self.file = nil
        try file.close()
        return rest
    }

    /// `samples` encoded and written, or with `nil` the packets the encoder still held.
    private func encode(_ samples: AVAudioPCMBuffer?) throws -> Data {
        guard let file else { return Data() }
        var input = samples
        var stream = Data()
        while true {
            packets.packetCount = 0
            packets.byteLength = 0
            var failure: NSError?
            let status = encoder.convert(to: packets, error: &failure) { _, state in
                if let next = input {
                    input = nil
                    state.pointee = .haveData
                    return next
                }
                state.pointee = samples == nil ? .endOfStream : .noDataNow
                return nil
            }
            if status == .error { throw failure ?? MigoRecordingError("the recording could not be encoded") }
            let count = Int(packets.packetCount)
            if count > 0, let descriptions = packets.packetDescriptions {
                for index in 0..<count {
                    let packet = descriptions[index]
                    let size = Int(packet.mDataByteSize)
                    stream.append(Self.header(payload: size, rateIndex: rateIndex, channels: channels))
                    stream.append(Data(bytes: packets.data + Int(packet.mStartOffset), count: size))
                }
            }
            guard status == .haveData, count > 0 else { break }
        }
        if !stream.isEmpty { try file.write(contentsOf: stream) }
        return stream
    }

    /// The ADTS header of one AAC-LC packet of `payload` bytes: MPEG-4, no CRC.
    static func header(payload: Int, rateIndex: Int, channels: Int) -> Data {
        let length = payload + 7
        return Data([
            0xFF, 0xF1,
            UInt8(0x40 | (rateIndex << 2) | (channels >> 2)),
            UInt8(((channels & 3) << 6) | (length >> 11)),
            UInt8((length >> 3) & 0xFF),
            UInt8(((length & 7) << 5) | 0x1F),
            0xFC,
        ])
    }
}

/// AAC in an MPEG-4 file, which the system writes whole: nothing of it is handed over before it is finished.
final class MigoMpeg4Sink: MigoRecordingSink {
    private var file: AVAudioFile?

    init(url: URL, samples: AVAudioFormat, bitRate: Int) throws {
        guard let aac = aacFormat(for: samples), let encoder = AVAudioConverter(from: samples, to: aac) else {
            throw MigoRecordingError("invalid audio parameters")
        }
        file = try AVAudioFile(
            forWriting: url,
            settings: [
                AVFormatIDKey: kAudioFormatMPEG4AAC,
                AVSampleRateKey: samples.sampleRate,
                AVNumberOfChannelsKey: samples.channelCount,
                AVEncoderBitRateKey: nearestBitRate(bitRate, of: encoder),
            ],
            commonFormat: .pcmFormatInt16, interleaved: true)
    }

    func write(_ samples: AVAudioPCMBuffer) throws -> Data? {
        try file?.write(from: samples)
        return nil
    }

    func finish() throws -> Data? {
        // Let go of, the file writes its index and closes.
        file = nil
        return nil
    }
}
