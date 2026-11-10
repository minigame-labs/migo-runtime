#if os(iOS)
    import AVFoundation
    import Foundation
    import Vision

    /// scanCode's kinds of code, as the camera and Vision name them: what `scanType` asks for -- `barCode`, `qrCode`,
    /// `datamatrix`, `pdf417`, none being the reference's default of bar codes and QR codes -- and the reference's name
    /// for a code read, as the Android SDK reports it (`QR_CODE`, `EAN_13`, ...).
    struct MigoCodeKinds: Equatable {
        enum Kind: String, CaseIterable {
            case qr = "QR_CODE"
            case aztec = "AZTEC"
            case ean8 = "EAN_8"
            case ean13 = "EAN_13"
            case upcE = "UPC_E"
            case code39 = "CODE_39"
            case code93 = "CODE_93"
            case code128 = "CODE_128"
            case itf = "ITF"
            case codabar = "CODABAR"
            case dataMatrix = "DATA_MATRIX"
            case pdf417 = "PDF_417"
        }

        private static let barCodes: Set<Kind> = [.ean8, .ean13, .upcE, .code39, .code93, .code128, .itf, .codabar]

        let kinds: Set<Kind>

        init(scanType: [Any]?) {
            var kinds: Set<Kind> = []
            for type in scanType ?? [] {
                switch type as? String {
                case "barCode": kinds.formUnion(Self.barCodes)
                case "qrCode": kinds.formUnion([.qr, .aztec])
                case "datamatrix": kinds.insert(.dataMatrix)
                case "pdf417": kinds.insert(.pdf417)
                default: break
                }
            }
            self.kinds = kinds.isEmpty ? Self.barCodes.union([.qr, .aztec]) : kinds
        }

        /// What the camera's metadata output reads, of these.
        var metadataTypes: [AVMetadataObject.ObjectType] {
            kinds.flatMap { kind -> [AVMetadataObject.ObjectType] in
                switch kind {
                case .qr: return [.qr]
                case .aztec: return [.aztec]
                case .ean8: return [.ean8]
                case .ean13: return [.ean13]
                case .upcE: return [.upce]
                case .code39: return [.code39, .code39Mod43]
                case .code93: return [.code93]
                case .code128: return [.code128]
                case .itf: return [.itf14, .interleaved2of5]
                case .codabar:
                    // The camera reads Codabar from iOS 15.4; Vision reads it in a picture from 15.
                    if #available(iOS 15.4, *) { return [.codabar] }
                    return []
                case .dataMatrix: return [.dataMatrix]
                case .pdf417: return [.pdf417]
                }
            }
        }

        /// What Vision reads in a picture, of these.
        var symbologies: [VNBarcodeSymbology] {
            kinds.flatMap { kind -> [VNBarcodeSymbology] in
                switch kind {
                case .qr: return [.qr]
                case .aztec: return [.aztec]
                case .ean8: return [.ean8]
                case .ean13: return [.ean13]
                case .upcE: return [.upce]
                case .code39: return [.code39, .code39Checksum, .code39FullASCII, .code39FullASCIIChecksum]
                case .code93: return [.code93, .code93i]
                case .code128: return [.code128]
                case .itf: return [.itf14, .i2of5, .i2of5Checksum]
                case .codabar: return [.codabar]
                case .dataMatrix: return [.dataMatrix]
                case .pdf417: return [.pdf417]
                }
            }
        }

        /// The reference's name for a code the camera read.
        static func name(of type: AVMetadataObject.ObjectType) -> String? {
            if #available(iOS 15.4, *), type == .codabar { return Kind.codabar.rawValue }
            switch type {
            case .qr: return Kind.qr.rawValue
            case .aztec: return Kind.aztec.rawValue
            case .ean8: return Kind.ean8.rawValue
            case .ean13: return Kind.ean13.rawValue
            case .upce: return Kind.upcE.rawValue
            case .code39, .code39Mod43: return Kind.code39.rawValue
            case .code93: return Kind.code93.rawValue
            case .code128: return Kind.code128.rawValue
            case .itf14, .interleaved2of5: return Kind.itf.rawValue
            case .dataMatrix: return Kind.dataMatrix.rawValue
            case .pdf417: return Kind.pdf417.rawValue
            default: return nil
            }
        }

        /// The reference's name for a code Vision read.
        static func name(of symbology: VNBarcodeSymbology) -> String? {
            switch symbology {
            case .qr: return Kind.qr.rawValue
            case .aztec: return Kind.aztec.rawValue
            case .ean8: return Kind.ean8.rawValue
            case .ean13: return Kind.ean13.rawValue
            case .upce: return Kind.upcE.rawValue
            case .code39, .code39Checksum, .code39FullASCII, .code39FullASCIIChecksum: return Kind.code39.rawValue
            case .code93, .code93i: return Kind.code93.rawValue
            case .code128: return Kind.code128.rawValue
            case .itf14, .i2of5, .i2of5Checksum: return Kind.itf.rawValue
            case .codabar: return Kind.codabar.rawValue
            case .dataMatrix: return Kind.dataMatrix.rawValue
            case .pdf417: return Kind.pdf417.rawValue
            default: return nil
            }
        }

        /// scanCode's answer for the first code of these kinds Vision reads in `image`, or `nil` with none.
        /// Synchronous: off the main thread.
        ///
        /// The newest detector reads first, on whatever compute the device offers its model. Where it reads nothing,
        /// the iOS 15 detector reads again on the CPU: a device that cannot run the newer model gets no codes from it
        /// and no error either -- the iOS Simulator, measured -- and a still picture costs one pass more.
        func read(_ image: CGImage, orientation: CGImagePropertyOrientation = .up) -> [String: Any]? {
            let handler = VNImageRequestHandler(cgImage: image, orientation: orientation)
            for older in [false, true] {
                let request = VNDetectBarcodesRequest()
                if older {
                    request.revision = VNDetectBarcodesRequestRevision2
                    Self.runOnCPU(request)
                }
                // After the revision, which puts the symbologies back to its own.
                request.symbologies = symbologies
                do {
                    try handler.perform([request])
                } catch {
                    NSLog("MigoCamera: revision \(request.revision) could not read the picture: \(error)")
                    continue
                }
                for observation in request.results ?? [] {
                    guard let text = observation.payloadStringValue, let name = Self.name(of: observation.symbology),
                        let kind = Kind(rawValue: name), kinds.contains(kind)
                    else { continue }
                    return Self.answer(text: text, scanType: name)
                }
            }
            return nil
        }

        /// Have `request` run on the CPU alone.
        private static func runOnCPU(_ request: VNRequest) {
            if #available(iOS 17.0, *) {
                if let devices = try? request.supportedComputeStageDevices[.main],
                    let cpu = devices.first(where: { if case .cpu = $0 { return true } else { return false } })
                {
                    request.setComputeDevice(cpu, for: .main)
                }
            } else {
                request.usesCPUOnly = true
            }
        }

        /// scanCode's answer for a code with `text` of the kind named `scanType`: its data is its text in UTF-8,
        /// as the Android SDK hands over a code with no byte segments.
        static func answer(text: String, scanType: String) -> [String: Any] {
            [
                "result": text, "scanType": scanType, "charSet": "UTF-8",
                // The reference's path is the page a code of its own leads to; no code here is one.
                "path": "", "rawData": Data(text.utf8).base64EncodedString(),
            ]
        }
    }
#endif
