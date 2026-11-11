#if os(iOS)
    import AVFoundation
    import CoreImage
    import Vision
    import XCTest

    @testable import MigoAppleCamera

    final class MigoCodeKindsTests: XCTestCase {
        func testNoScanTypeIsTheReferencesDefaultOfBarCodesAndQrCodes() {
            let kinds = MigoCodeKinds(scanType: nil)
            XCTAssertTrue(kinds.kinds.contains(.qr))
            XCTAssertTrue(kinds.kinds.contains(.ean13))
            XCTAssertFalse(kinds.kinds.contains(.dataMatrix))
            XCTAssertEqual(kinds, MigoCodeKinds(scanType: ["unknown"]), "a name it does not know is none")
        }

        func testScanTypeNamesTheKindsTheCameraAndVisionRead() {
            let kinds = MigoCodeKinds(scanType: ["qrCode", "pdf417"])
            XCTAssertEqual(kinds.kinds, [.qr, .aztec, .pdf417])
            XCTAssertEqual(Set(kinds.metadataTypes), [.qr, .aztec, .pdf417])
            XCTAssertEqual(Set(kinds.symbologies), [.qr, .aztec, .pdf417])
            XCTAssertTrue(Set(MigoCodeKinds(scanType: ["barCode"]).symbologies).contains(.code128))
        }

        func testACodeReadIsNamedAsTheReferenceNamesIt() {
            XCTAssertEqual(MigoCodeKinds.name(of: AVMetadataObject.ObjectType.qr), "QR_CODE")
            XCTAssertEqual(MigoCodeKinds.name(of: AVMetadataObject.ObjectType.code39Mod43), "CODE_39")
            XCTAssertEqual(MigoCodeKinds.name(of: AVMetadataObject.ObjectType.interleaved2of5), "ITF")
            XCTAssertEqual(MigoCodeKinds.name(of: VNBarcodeSymbology.ean13), "EAN_13")
            XCTAssertEqual(MigoCodeKinds.name(of: VNBarcodeSymbology.dataMatrix), "DATA_MATRIX")
            XCTAssertNil(MigoCodeKinds.name(of: AVMetadataObject.ObjectType.face))
        }

        func testTheAnswerCarriesTheCodesTextAndItsDataInBase64() {
            let answer = MigoCodeKinds.answer(text: "migo-scan-spec", scanType: "QR_CODE")
            XCTAssertEqual(answer["result"] as? String, "migo-scan-spec")
            XCTAssertEqual(answer["scanType"] as? String, "QR_CODE")
            XCTAssertEqual(answer["charSet"] as? String, "UTF-8")
            XCTAssertEqual(answer["path"] as? String, "")
            XCTAssertEqual(answer["rawData"] as? String, "bWlnby1zY2FuLXNwZWM=")
        }

        /// A QR code as CoreImage draws it, ten pixels a module on white.
        private func qr(_ text: String) throws -> CGImage {
            let filter = try XCTUnwrap(CIFilter(name: "CIQRCodeGenerator"))
            filter.setValue(Data(text.utf8), forKey: "inputMessage")
            filter.setValue("M", forKey: "inputCorrectionLevel")
            let modules = try XCTUnwrap(filter.outputImage).transformed(by: CGAffineTransform(scaleX: 10, y: 10))
            let frame = modules.extent.insetBy(dx: -40, dy: -40)
            let code = modules.composited(over: CIImage(color: .white).cropped(to: frame))
            return try XCTUnwrap(CIContext().createCGImage(code, from: frame))
        }

        func testAPicturesCodeIsReadAsTheAlbumReadsIt() throws {
            let answer = try XCTUnwrap(MigoCodeKinds(scanType: ["qrCode"]).read(try qr("migo-scan-spec")))
            XCTAssertEqual(answer["result"] as? String, "migo-scan-spec")
            XCTAssertEqual(answer["scanType"] as? String, "QR_CODE")
        }

        func testACodeOfAKindNotAskedForIsNotRead() throws {
            XCTAssertNil(MigoCodeKinds(scanType: ["barCode"]).read(try qr("migo")))
        }

        func testThePlayersLanguageIsTheScannersAndEveryWordIsTranslated() throws {
            for language in ["en", "zh-Hans"] {
                let path = try XCTUnwrap(MigoScannerText.bundle.path(forResource: language, ofType: "lproj"), language)
                let table = try XCTUnwrap(NSDictionary(contentsOfFile: path + "/Localizable.strings") as? [String: String])
                XCTAssertEqual(
                    Set(table.keys), ["scan.hint", "scan.album", "scan.cancel", "scan.noCode", "scan.noCamera"], language)
            }
        }
    }
#endif
