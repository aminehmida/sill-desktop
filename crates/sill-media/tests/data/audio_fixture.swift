// Makes audio_eld_480.sill43 and audio_eld_512.sill43 (and their .at.f32), the fixtures of sill-media's audio tests and of
// examples/audio_tone.rs: two seconds of a tone as Sill for Mac 0.6.1 sends it (kind 43), encoded with
// the encoder it uses (AudioToolbox's AAC-ELD, 48 kHz stereo, 128 kbps, one packet per block of 480 or
// 512 frames, the converter's own magic cookie and priming). macOS only:
//
//     swiftc -O audio_fixture.swift -o /tmp/audio_fixture && /tmp/audio_fixture .
//
// The file: "SILL43\0\1", then records of
//     length UInt32 · stamp Float64 · payload          (big endian)
// where the payload is a kind 43 payload (type 1: the format's JSON; type 2: packets) and the stamp is
// the message header's, in seconds from the tone's first sample (a player adds its wall clock).
//
// The tone: left 1000 Hz, right 1500 Hz, 0.4 of full scale, 96,000 frames, the first and last 480
// under a raised cosine. Frame i of it is frame i + priming of the decoded packets.
//
// audio_eld_*.at.f32: the first ten packets as AudioToolbox's own decoder decodes them (interleaved
// Float32, little endian), to hold FFmpeg's decoder to.
import AudioToolbox
import Foundation

let rate = 48_000.0
let frames = 96_000

func tone() -> [Float] {
    var x = [Float](repeating: 0, count: frames * 2)
    for i in 0..<frames {
        var g = 0.4
        let edge = min(i, frames - 1 - i)
        if edge < 480 { g *= 0.5 - 0.5 * cos(Double.pi * Double(edge + 1) / 481) }
        x[2 * i] = Float(g * sin(2 * Double.pi * 1000 * Double(i) / rate))
        x[2 * i + 1] = Float(g * sin(2 * Double.pi * 1500 * Double(i) / rate))
    }
    return x
}

final class Feed {
    var samples: UnsafeRawPointer?
    var frames = 0
}
let noMoreInput: OSStatus = 0x6E6F_6D6F
let supply: AudioConverterComplexInputDataProc = { _, count, data, _, context in
    let feed = Unmanaged<Feed>.fromOpaque(context!).takeUnretainedValue()
    guard let samples = feed.samples, feed.frames > 0 else {
        count.pointee = 0
        return noMoreInput
    }
    let n = min(Int(count.pointee), feed.frames)
    count.pointee = UInt32(n)
    data.pointee.mNumberBuffers = 1
    data.pointee.mBuffers.mNumberChannels = 2
    data.pointee.mBuffers.mDataByteSize = UInt32(n * 8)
    data.pointee.mBuffers.mData = UnsafeMutableRawPointer(mutating: samples)
    feed.samples = samples.advanced(by: n * 8)
    feed.frames -= n
    return noErr
}

func check(_ status: OSStatus, _ what: String) {
    if status != noErr { print("\(what) failed (\(status))"); exit(1) }
}

extension Data {
    mutating func be<T: FixedWidthInteger>(_ v: T) { let b = Swift.withUnsafeBytes(of: v.bigEndian) { Array($0) }; append(contentsOf: b) }
}

func make(framesPerPacket F: Int, to path: String) {
    var input = AudioStreamBasicDescription(mSampleRate: rate, mFormatID: kAudioFormatLinearPCM,
                                            mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                                            mBytesPerPacket: 8, mFramesPerPacket: 1, mBytesPerFrame: 8,
                                            mChannelsPerFrame: 2, mBitsPerChannel: 32, mReserved: 0)
    var out = AudioStreamBasicDescription()
    out.mFormatID = kAudioFormatMPEG4AAC_ELD
    out.mSampleRate = rate
    out.mChannelsPerFrame = 2
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    check(AudioFormatGetProperty(kAudioFormatProperty_FormatInfo, 0, nil, &size, &out), "AAC-ELD's format")
    out.mFramesPerPacket = UInt32(F)
    var made: AudioConverterRef?
    check(AudioConverterNew(&input, &out, &made), "AudioConverterNew")
    let converter = made!
    var bitrate: UInt32 = 128_000
    check(AudioConverterSetProperty(converter, kAudioConverterEncodeBitRate, 4, &bitrate), "the bitrate")
    var got = AudioStreamBasicDescription()
    size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    check(AudioConverterGetProperty(converter, kAudioConverterCurrentOutputStreamDescription, &size, &got), "the output format")
    var prime = AudioConverterPrimeInfo()
    size = UInt32(MemoryLayout<AudioConverterPrimeInfo>.size)
    check(AudioConverterGetProperty(converter, kAudioConverterPrimeInfo, &size, &prime), "the priming")
    var cookieSize: UInt32 = 0
    check(AudioConverterGetPropertyInfo(converter, kAudioConverterCompressionMagicCookie, &cookieSize, nil), "the cookie's size")
    var cookie = Data(count: Int(cookieSize))
    check(cookie.withUnsafeMutableBytes { AudioConverterGetProperty(converter, kAudioConverterCompressionMagicCookie, &cookieSize, $0.baseAddress!) }, "the cookie")
    cookie = cookie.prefix(Int(cookieSize))
    var maxPacket: UInt32 = 0
    size = 4
    check(AudioConverterGetProperty(converter, kAudioConverterPropertyMaximumOutputPacketSize, &size, &maxPacket), "the packet size")
    let P = Int(prime.leadingFrames)
    precondition(Int(got.mFramesPerPacket) == F)

    var file = Data("SILL43".utf8)
    file.append(contentsOf: [0, 1])
    func record(_ stamp: Double, _ payload: Data) {
        file.be(UInt32(payload.count))
        file.be(stamp.bitPattern)
        file.append(payload)
    }
    // The format, as Wire.encode writes an AudioFormat (JSONEncoder; sorted here so the file is stable).
    let format: [String: Any] = ["codec": "aac-eld", "sampleRate": 48000, "channels": 2, "framesPerPacket": F,
                                 "primingFrames": P, "bitrate": Int(bitrate), "epoch": 1,
                                 "cookie": cookie.base64EncodedString(), "source": "test"]
    var payload = Data([1])
    payload.append(try! JSONSerialization.data(withJSONObject: format, options: [.sortedKeys]))
    record(0, payload)

    let x = tone()
    let blocks = (frames + P + F - 1) / F
    let feed = Feed()
    let output = UnsafeMutableRawPointer.allocate(byteCount: Int(maxPacket), alignment: 16)
    var bytes = 0
    var encoded: [Data] = []
    for n in 0..<blocks {
        var block = [Float](repeating: 0, count: F * 2)
        let from = n * F
        for i in 0..<F where from + i < frames {
            block[2 * i] = x[2 * (from + i)]
            block[2 * i + 1] = x[2 * (from + i) + 1]
        }
        let packet: Data = block.withUnsafeBufferPointer { samples in
            feed.samples = UnsafeRawPointer(samples.baseAddress)
            feed.frames = F
            var packets: UInt32 = 1
            var description = AudioStreamPacketDescription()
            var list = AudioBufferList(mNumberBuffers: 1, mBuffers: AudioBuffer(mNumberChannels: 2, mDataByteSize: maxPacket, mData: output))
            let status = AudioConverterFillComplexBuffer(converter, supply, Unmanaged.passUnretained(feed).toOpaque(), &packets, &list, &description)
            precondition((status == noErr || status == noMoreInput) && packets == 1, "packet \(n): \(status)")
            return Data(bytes: output, count: Int(list.mBuffers.mDataByteSize))
        }
        bytes += packet.count
        encoded.append(packet)
        // type 2 · epoch · seq · flags · count · length · bytes; stamped with its first decoded sample.
        var p = Data([2])
        p.be(UInt16(1)); p.be(UInt32(n)); p.append(n == 0 ? 1 : 0); p.append(1)
        p.be(UInt16(packet.count)); p.append(packet)
        record(Double(n * F - P) / rate, p)
    }
    AudioConverterDispose(converter)
    try! file.write(to: URL(fileURLWithPath: path))
    // What Apple's own decoder makes of the first ten packets (as iOSClient's AudioDecoder decodes
    // them): Float32, little endian, left and right interleaved.
    let reference = decode(encoded.prefix(10), framesPerPacket: F, cookie: cookie)
    try! reference.write(to: URL(fileURLWithPath: path.replacingOccurrences(of: ".sill43", with: ".at.f32")))
    print("\(path): \(F) frames a packet, priming \(P), \(blocks) packets, \(bytes) bytes of AAC-ELD, cookie \(cookie.map { String(format: "%02x", $0) }.joined())")
}

final class PacketFeed {
    var packet: UnsafeRawPointer?
    var bytes = 0
    let description = UnsafeMutablePointer<AudioStreamPacketDescription>.allocate(capacity: 1)
}
let supplyPacket: AudioConverterComplexInputDataProc = { _, count, data, descriptions, context in
    let feed = Unmanaged<PacketFeed>.fromOpaque(context!).takeUnretainedValue()
    guard let packet = feed.packet, feed.bytes > 0 else {
        count.pointee = 0
        return noMoreInput
    }
    count.pointee = 1
    data.pointee.mNumberBuffers = 1
    data.pointee.mBuffers.mNumberChannels = 2
    data.pointee.mBuffers.mDataByteSize = UInt32(feed.bytes)
    data.pointee.mBuffers.mData = UnsafeMutableRawPointer(mutating: packet)
    feed.description.pointee = AudioStreamPacketDescription(mStartOffset: 0, mVariableFramesInPacket: 0, mDataByteSize: UInt32(feed.bytes))
    descriptions?.pointee = feed.description
    feed.packet = nil
    feed.bytes = 0
    return noErr
}

func decode(_ packets: ArraySlice<Data>, framesPerPacket F: Int, cookie: Data) -> Data {
    var input = AudioStreamBasicDescription()
    input.mFormatID = kAudioFormatMPEG4AAC_ELD
    input.mSampleRate = rate
    input.mChannelsPerFrame = 2
    var size = UInt32(MemoryLayout<AudioStreamBasicDescription>.size)
    check(AudioFormatGetProperty(kAudioFormatProperty_FormatInfo, 0, nil, &size, &input), "AAC-ELD's format")
    input.mFramesPerPacket = UInt32(F)
    var output = AudioStreamBasicDescription(mSampleRate: rate, mFormatID: kAudioFormatLinearPCM,
                                             mFormatFlags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked,
                                             mBytesPerPacket: 8, mFramesPerPacket: 1, mBytesPerFrame: 8,
                                             mChannelsPerFrame: 2, mBitsPerChannel: 32, mReserved: 0)
    var made: AudioConverterRef?
    check(AudioConverterNew(&input, &output, &made), "the decoder")
    let converter = made!
    check(cookie.withUnsafeBytes { AudioConverterSetProperty(converter, kAudioConverterDecompressionMagicCookie, UInt32(cookie.count), $0.baseAddress!) }, "the decoder's cookie")
    let feed = PacketFeed()
    var pcm = Data()
    let buffer = UnsafeMutableRawPointer.allocate(byteCount: F * 8, alignment: 16)
    for packet in packets {
        packet.withUnsafeBytes { bytes in
            feed.packet = bytes.baseAddress
            feed.bytes = bytes.count
            var frames = UInt32(F)
            var list = AudioBufferList(mNumberBuffers: 1, mBuffers: AudioBuffer(mNumberChannels: 2, mDataByteSize: UInt32(F * 8), mData: buffer))
            let status = AudioConverterFillComplexBuffer(converter, supplyPacket, Unmanaged.passUnretained(feed).toOpaque(), &frames, &list, nil)
            precondition((status == noErr || status == noMoreInput) && Int(frames) == F, "decoding: \(status), \(frames) frames")
            pcm.append(Data(bytes: buffer, count: F * 8))
        }
    }
    AudioConverterDispose(converter)
    return pcm
}

let dir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."
make(framesPerPacket: 480, to: dir + "/audio_eld_480.sill43")
make(framesPerPacket: 512, to: dir + "/audio_eld_512.sill43")
