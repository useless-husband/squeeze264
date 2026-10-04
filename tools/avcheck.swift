// Opens an MP4 with AVFoundation (the framework QuickTime Player uses),
// reports whether it is playable, and optionally writes every decoded frame
// as planar I420 so it can be compared with the encoder's reconstruction.
//
// Usage: swift tools/avcheck.swift <file.mp4> [decoded.yuv]
// macOS only.

import AVFoundation
import Foundation

let args = CommandLine.arguments
guard args.count >= 2 else {
    FileHandle.standardError.write("usage: avcheck.swift <file.mp4> [decoded.yuv]\n".data(using: .utf8)!)
    exit(2)
}
let asset = AVURLAsset(url: URL(fileURLWithPath: args[1]))

Task {
    do {
        let playable = try await asset.load(.isPlayable)
        let duration = try await asset.load(.duration)
        guard let track = try await asset.loadTracks(withMediaType: .video).first else {
            print("error: no video track")
            exit(1)
        }
        let size = try await track.load(.naturalSize)
        let fps = try await track.load(.nominalFrameRate)

        // Ask for the decoder's native 4:2:0 video-range output so that no
        // colour conversion touches the samples.
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(
            track: track,
            outputSettings: [
                kCVPixelBufferPixelFormatTypeKey as String:
                    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
            ])
        output.alwaysCopiesSampleData = false
        reader.add(output)
        guard reader.startReading() else {
            print("error: \(String(describing: reader.error))")
            exit(1)
        }

        var sink: FileHandle? = nil
        if args.count >= 3 {
            FileManager.default.createFile(atPath: args[2], contents: nil)
            sink = FileHandle(forWritingAtPath: args[2])
        }
        var frames = 0
        while let sample = output.copyNextSampleBuffer() {
            guard let pb = CMSampleBufferGetImageBuffer(sample) else { continue }
            frames += 1
            guard let sink = sink else { continue }
            CVPixelBufferLockBaseAddress(pb, .readOnly)
            let w = CVPixelBufferGetWidth(pb)
            let h = CVPixelBufferGetHeight(pb)
            var data = Data(capacity: w * h * 3 / 2)
            let yBase = CVPixelBufferGetBaseAddressOfPlane(pb, 0)!.assumingMemoryBound(to: UInt8.self)
            let yStride = CVPixelBufferGetBytesPerRowOfPlane(pb, 0)
            for row in 0..<h {
                data.append(yBase + row * yStride, count: w)
            }
            // De-interleave the CbCr plane into separate U and V planes.
            let cBase = CVPixelBufferGetBaseAddressOfPlane(pb, 1)!.assumingMemoryBound(to: UInt8.self)
            let cStride = CVPixelBufferGetBytesPerRowOfPlane(pb, 1)
            let (cw, ch) = (w / 2, h / 2)
            var u = [UInt8](repeating: 0, count: cw * ch)
            var v = [UInt8](repeating: 0, count: cw * ch)
            for row in 0..<ch {
                for col in 0..<cw {
                    u[row * cw + col] = cBase[row * cStride + 2 * col]
                    v[row * cw + col] = cBase[row * cStride + 2 * col + 1]
                }
            }
            data.append(contentsOf: u)
            data.append(contentsOf: v)
            CVPixelBufferUnlockBaseAddress(pb, .readOnly)
            sink.write(data)
        }
        try? sink?.close()
        let ok = playable && reader.status == .completed
        print(
            "playable=\(playable) size=\(Int(size.width))x\(Int(size.height)) "
                + "fps=\(String(format: "%.3f", fps)) duration=\(String(format: "%.3f", CMTimeGetSeconds(duration))) "
                + "frames=\(frames) reader=\(reader.status == .completed ? "completed" : "failed")")
        exit(ok ? 0 : 1)
    } catch {
        print("error: \(error)")
        exit(1)
    }
}
dispatchMain()
