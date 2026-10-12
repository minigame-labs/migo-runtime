import Foundation
import MigoEngine
import Photos
import XCTest

@testable import MigoApplePhotos
@testable import MigoAppleRenderer

/// The photo library's grant in the engine's words, and what the capability
/// claims: the album scope, and no whole service.
final class MigoPhotoLibraryTests: XCTestCase {
    func testTheGrantInTheEnginesWords() {
        XCTAssertEqual(MigoPhotoLibrary.authorization(.notDetermined), UInt8(MIGO_AUTHORIZATION_NOT_DETERMINED))
        XCTAssertEqual(MigoPhotoLibrary.authorization(.denied), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoPhotoLibrary.authorization(.restricted), UInt8(MIGO_AUTHORIZATION_DENIED))
        XCTAssertEqual(MigoPhotoLibrary.authorization(.authorized), UInt8(MIGO_AUTHORIZATION_AUTHORIZED))
        // Adding to a limited library is adding to the library.
        XCTAssertEqual(MigoPhotoLibrary.authorization(.limited), UInt8(MIGO_AUTHORIZATION_AUTHORIZED))
        XCTAssertEqual(MigoPhotoLibrary.scopeState(.notDetermined), MigoScopeState(MIGO_SCOPE_STATE_UNKNOWN))
        XCTAssertEqual(MigoPhotoLibrary.scopeState(.denied), MigoScopeState(MIGO_SCOPE_STATE_DENIED))
        XCTAssertEqual(MigoPhotoLibrary.scopeState(.authorized), MigoScopeState(MIGO_SCOPE_STATE_GRANTED))
    }

    func testItDecidesTheAlbumScopeAndAnswersNoWholeService() {
        let library = MigoPhotoLibrary()
        XCTAssertEqual(library.scopes, [MigoScope(MIGO_SCOPE_WRITE_PHOTOS_ALBUM)])
        XCTAssertTrue(library.services.isEmpty)
        var answers: [Bool] = []
        library.requestScope(MigoScope(MIGO_SCOPE_CAMERA)) { answers.append($0) }
        XCTAssertEqual(answers, [false])
    }
}
