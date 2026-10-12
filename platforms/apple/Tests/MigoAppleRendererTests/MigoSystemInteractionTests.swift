import Foundation
import XCTest

@testable import MigoAppleRenderer

#if os(macOS)
    import AppKit
#endif

/// The interaction service's semantics over a recording presenter, and on a Mac
/// the system presenter itself, driven through a real window's sheets.
final class MigoSystemInteractionTests: XCTestCase {
    private final class Recorder: @unchecked Sendable {
        var outcomes: [MigoHostServiceReply.Outcome] = []
    }

    private final class FakePresenter: MigoInteractionPresenter {
        var shown: [MigoHud?] = []
        var modal: MigoModal?
        var modalAnswer: MigoModalAnswer?? = .some(MigoModalAnswer(confirmed: true, content: nil))
        var sheet: (title: String, items: [String])?
        var sheetAnswer: Int?? = .some(0)
        func showHud(_ hud: MigoHud) { shown.append(hud) }
        func hideHud() { shown.append(nil) }
        func showModal(_ modal: MigoModal, completion: @escaping (MigoModalAnswer?) -> Void) {
            self.modal = modal
            if let answer = modalAnswer { completion(answer) }
        }
        func showActionSheet(title: String, items: [String], completion: @escaping (Int?) -> Void) {
            sheet = (title, items)
            if let answer = sheetAnswer { completion(answer) }
        }
    }

    private func request(_ method: UInt32, _ object: [String: Any] = [:], _ recorder: Recorder? = nil)
        -> MigoHostServiceRequest
    {
        MigoHostServiceRequest(
            service: .interaction, method: method, payload: try! JSONSerialization.data(withJSONObject: object),
            reply: recorder.map { recorder in MigoHostServiceReply { recorder.outcomes.append($0) } })
    }

    private func object(_ outcome: MigoHostServiceReply.Outcome) -> [String: Any]? {
        guard case .success(let json) = outcome else { return nil }
        return (try? JSONSerialization.jsonObject(with: json)) as? [String: Any]
    }

    func testOneIndicatorAtATimeAndEachHideHidesOnlyItsOwnKind() {
        let presenter = FakePresenter()
        let interaction = MigoSystemInteraction(presenter: presenter)
        typealias Method = MigoHostServiceID.Interaction
        interaction.handle(request(Method.showLoading, ["title": "Loading", "mask": true]))
        interaction.handle(request(Method.hideToast))
        interaction.handle(request(Method.showToast, ["title": "Saved", "icon": "error", "duration": 60_000]))
        interaction.handle(request(Method.hideLoading))
        interaction.handle(request(Method.hideToast))
        XCTAssertEqual(
            presenter.shown,
            [
                MigoHud(kind: .loading, title: "Loading", mask: true),
                // A toast replaces the loading indicator; hiding the loading
                // indicator then leaves the toast, hiding the toast hides it.
                MigoHud(kind: .toast(icon: "error"), title: "Saved", mask: false),
                nil,
            ])
    }

    func testAToastHidesItselfAfterItsDuration() {
        let presenter = FakePresenter()
        let interaction = MigoSystemInteraction(presenter: presenter)
        interaction.handle(request(MigoHostServiceID.Interaction.showToast, ["title": "Hi", "duration": 50]))
        let hidden = expectation(description: "the toast hides itself")
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3) {
            if presenter.shown.last == .some(nil) { hidden.fulfill() }
        }
        wait(for: [hidden], timeout: 2)
        XCTAssertEqual(presenter.shown.first??.kind, .toast(icon: "success"), "the default icon is success")
    }

    func testAModalAnswersConfirmAndCancelAndAnEditableOnesText() {
        let presenter = FakePresenter()
        let interaction = MigoSystemInteraction(presenter: presenter)
        let recorder = Recorder()
        typealias Method = MigoHostServiceID.Interaction
        interaction.handle(request(Method.showModal, ["title": "Quit?", "content": "Progress is saved"], recorder))
        XCTAssertEqual(presenter.modal?.showCancel, true)
        XCTAssertEqual(presenter.modal?.cancelText, MigoSystemInteraction.cancel)
        presenter.modalAnswer = .some(MigoModalAnswer(confirmed: true, content: "Mira"))
        interaction.handle(
            request(Method.showModal, ["title": "Name", "editable": true, "placeholderText": "your name"], recorder))
        XCTAssertEqual(presenter.modal?.editable, true)
        XCTAssertEqual(presenter.modal?.placeholderText, "your name")
        presenter.modalAnswer = .some(nil)
        interaction.handle(request(Method.showModal, ["title": "Nowhere"], recorder))
        XCTAssertEqual(recorder.outcomes.count, 3)
        XCTAssertEqual(object(recorder.outcomes[0])?["confirm"] as? Bool, true)
        XCTAssertEqual(object(recorder.outcomes[0])?["cancel"] as? Bool, false)
        XCTAssertNil(object(recorder.outcomes[0])?["content"], "only an editable modal answers content")
        XCTAssertEqual(object(recorder.outcomes[1])?["content"] as? String, "Mira")
        XCTAssertEqual(recorder.outcomes[2], .failure(message: "there is no window to show it in", code: nil))
    }

    func testAnActionSheetAnswersTheTappedIndexOrCancel() {
        let presenter = FakePresenter()
        let interaction = MigoSystemInteraction(presenter: presenter)
        let recorder = Recorder()
        let method = MigoHostServiceID.Interaction.showActionSheet
        presenter.sheetAnswer = .some(1)
        interaction.handle(request(method, ["alertText": "Share", "itemList": ["Friends", "Moments"]], recorder))
        XCTAssertEqual(presenter.sheet?.items, ["Friends", "Moments"])
        presenter.sheetAnswer = .some(-1)
        interaction.handle(request(method, ["itemList": ["A"]], recorder))
        interaction.handle(request(method, ["itemList": []], recorder))
        XCTAssertEqual(object(recorder.outcomes[0])?["tapIndex"] as? Int, 1)
        XCTAssertEqual(recorder.outcomes[1], .failure(message: "cancel", code: nil))
        XCTAssertEqual(recorder.outcomes[2], .failure(message: "itemList is empty", code: nil))
    }

    #if os(macOS)
        private func window() -> NSWindow {
            let window = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 400, height: 300), styleMask: [.titled],
                backing: .buffered, defer: false)
            window.isReleasedWhenClosed = false
            return window
        }

        /// The sheet's alert buttons, by title.
        private func endSheet(on window: NSWindow, pressing title: String, typing text: String? = nil) throws {
            let sheet = try XCTUnwrap(window.attachedSheet, "no sheet was shown")
            func views(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(views) }
            let all = views(try XCTUnwrap(sheet.contentView))
            if let text {
                let field = try XCTUnwrap(all.compactMap { $0 as? NSTextField }.first { $0.isEditable })
                field.stringValue = text
            }
            let button = try XCTUnwrap(all.compactMap { $0 as? NSButton }.first { $0.title == title })
            button.performClick(nil)
        }

        func testTheMacPresenterAnswersThroughTheWindowsSheets() throws {
            let window = window()
            let presenter = MigoSystemInteractionPresenter(window: { window })
            var answer: MigoModalAnswer??
            presenter.showModal(
                MigoModal(
                    title: "Name", content: "Mira", showCancel: true, cancelText: "Cancel", confirmText: "OK",
                    editable: true, placeholderText: "")
            ) { answer = .some($0) }
            try endSheet(on: window, pressing: "OK", typing: "Ada")
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
            XCTAssertEqual(answer, .some(MigoModalAnswer(confirmed: true, content: "Ada")))

            var tapped: Int??
            presenter.showActionSheet(title: "Pick", items: ["One", "Two"]) { tapped = .some($0) }
            try endSheet(on: window, pressing: "Two")
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
            XCTAssertEqual(tapped, .some(1))
            presenter.showActionSheet(title: "Pick", items: ["One"]) { tapped = .some($0) }
            try endSheet(on: window, pressing: MigoSystemInteraction.cancel)
            RunLoop.main.run(until: Date().addingTimeInterval(0.2))
            XCTAssertEqual(tapped, .some(-1))
        }

        func testWithNoWindowNothingIsShownAndTheCallSaysSo() {
            let presenter = MigoSystemInteractionPresenter(window: { nil })
            var answer: MigoModalAnswer?? = .none
            presenter.showModal(
                MigoModal(
                    title: "", content: "", showCancel: false, cancelText: "", confirmText: "OK", editable: false,
                    placeholderText: "")
            ) { answer = .some($0) }
            XCTAssertEqual(answer, .some(nil))
        }

        func testTheIndicatorCoversTheWindowAndMasksOnlyWhenAsked() throws {
            let window = window()
            let presenter = MigoSystemInteractionPresenter(window: { window })
            let content = try XCTUnwrap(window.contentView)
            presenter.showHud(MigoHud(kind: .toast(icon: "none"), title: "Saved", mask: false))
            let overlay = try XCTUnwrap(content.subviews.last as? MigoHudOverlay)
            XCTAssertNil(overlay.hitTest(NSPoint(x: 5, y: 5)), "clicks go through without a mask")
            presenter.showHud(MigoHud(kind: .loading, title: "", mask: true))
            XCTAssertEqual(content.subviews.filter { $0 is MigoHudOverlay }.count, 1, "one indicator at a time")
            let masked = try XCTUnwrap(content.subviews.last as? MigoHudOverlay)
            XCTAssertNotNil(masked.hitTest(NSPoint(x: 5, y: 5)), "a mask takes the clicks")
            presenter.hideHud()
            XCTAssertFalse(content.subviews.contains { $0 is MigoHudOverlay })
        }
    #endif
}
