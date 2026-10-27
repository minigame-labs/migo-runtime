import CoreGraphics
import Foundation
import ImageIO
import XCTest

@testable import MigoAppleRenderer

/// The parts of `createVideo`'s players that read content's options. What they
/// do as they play is migo-conformance's `video-spec` and `video-controls-spec`,
/// run against these players on macOS and the iOS simulator.
final class MigoVideoPlayersTests: XCTestCase {
    /// `backgroundColor` as the Android SDK reads it: `#RRGGBB`, or `#AARRGGBB`
    /// with the alpha first; anything else is not a colour.
    func testABackgroundColourIsReadAsTheAndroidSdkReadsIt() throws {
        let opaque = try XCTUnwrap(MigoVideoPlayer.color("#FF8000"))
        XCTAssertEqual(opaque.components, [1, 128.0 / 255, 0, 1])
        let translucent = try XCTUnwrap(MigoVideoPlayer.color("#80000000"))
        XCTAssertEqual(translucent.components, [0, 0, 0, 128.0 / 255])
        for text in ["", "red", "#FFF", "#GG0000", "000000"] {
            XCTAssertNil(MigoVideoPlayer.color(text), text)
        }
    }

    /// A source is a file content may open -- the path the engine exported --
    /// or an http(s) URL the engine allowed; nothing else is opened.
    func testASourceIsAFileOrAnHttpUrl() {
        XCTAssertEqual(MigoVideoPlayer.url("/data/game/movie.mp4"), URL(fileURLWithPath: "/data/game/movie.mp4"))
        XCTAssertEqual(MigoVideoPlayer.url("https://cdn.example.com/a.mp4")?.host, "cdn.example.com")
        XCTAssertNotNil(MigoVideoPlayer.url("http://192.168.1.2/a.mp4"))
        XCTAssertNil(MigoVideoPlayer.url("movie.mp4"))
        XCTAssertNil(MigoVideoPlayer.url("javascript:alert(1)"))
        XCTAssertNil(MigoVideoPlayer.url("ftp://example.com/a.mp4"))
    }

    /// The controls' clock: `m:ss`, `h:mm:ss` from an hour, as the Android SDK
    /// shows it -- and nothing odd for a duration it does not know yet.
    func testTheClockReadsAsTheAndroidSdksDoes() {
        XCTAssertEqual(MigoVideoControls.clock(0), "0:00")
        XCTAssertEqual(MigoVideoControls.clock(9.99), "0:09")
        XCTAssertEqual(MigoVideoControls.clock(75), "1:15")
        XCTAssertEqual(MigoVideoControls.clock(3725), "1:02:05")
        XCTAssertEqual(MigoVideoControls.clock(.nan), "0:00")
        XCTAssertEqual(MigoVideoControls.clock(-3), "0:00")
    }

    /// A poster is decoded only up to its bound, so a game cannot have the
    /// player hold an image of any size it names.
    func testAPosterPastItsBoundIsNotDecoded() throws {
        let image = try XCTUnwrap(Self.png())
        XCTAssertNotNil(MigoVideoPlayer.decode(image))
        var padded = image
        padded.append(Data(count: MigoVideoPlayer.maxPosterBytes))
        XCTAssertNil(MigoVideoPlayer.decode(padded))
        XCTAssertNil(MigoVideoPlayer.decode(Data("not an image".utf8)))
        XCTAssertNil(MigoVideoPlayer.decode(nil))
    }

    /// A 2x2 PNG.
    private static func png() -> Data? {
        let space = CGColorSpaceCreateDeviceRGB()
        guard
            let context = CGContext(
                data: nil, width: 2, height: 2, bitsPerComponent: 8, bytesPerRow: 8, space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return nil }
        context.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        context.fill(CGRect(x: 0, y: 0, width: 2, height: 2))
        guard let image = context.makeImage() else { return nil }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, "public.png" as CFString, 1, nil) else {
            return nil
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { return nil }
        return data as Data
    }
}
