import Foundation

/// A toast or a loading indicator, as content asked for it.
struct MigoHud: Equatable {
    enum Kind: Equatable {
        /// `showToast`, with its icon: `success`, `error`, `loading` or `none`.
        case toast(icon: String)
        /// `showLoading`.
        case loading
    }

    var kind: Kind
    var title: String
    /// Whether touches under it are held back from the game.
    var mask: Bool
}

/// A modal, as content asked for it.
struct MigoModal: Equatable {
    var title: String
    var content: String
    var showCancel: Bool
    var cancelText: String
    var confirmText: String
    /// An input in place of the content, its text the answer's `content`.
    var editable: Bool
    var placeholderText: String
}

/// What the player did with a modal.
struct MigoModalAnswer: Equatable {
    var confirmed: Bool
    /// The input's text, for an editable modal.
    var content: String?
}

/// Shows the system's own dialogs and indicators. A presenter that has nowhere
/// to show one -- no window yet -- answers `nil`.
protocol MigoInteractionPresenter: AnyObject {
    func showHud(_ hud: MigoHud)
    func hideHud()
    func showModal(_ modal: MigoModal, completion: @escaping (MigoModalAnswer?) -> Void)
    /// `completion` gets the index of the item tapped, `-1` for a dismissed
    /// sheet, `nil` when it could not be shown.
    func showActionSheet(title: String, items: [String], completion: @escaping (Int?) -> Void)
}

/// The interaction service, answered with the system's own UI: a toast or a
/// loading indicator over the game, a modal, an action sheet.
///
/// A toast and a loading indicator are one at a time, as on the common
/// platform: either replaces the other, `hideToast` hides only a toast and
/// `hideLoading` only a loading indicator, and a toast hides itself after its
/// duration. Button colours are not offered by the system's dialogs and are not
/// applied.
final class MigoSystemInteraction {
    private let presenter: MigoInteractionPresenter
    /// What is shown, so hiding the other kind leaves it.
    private var shown: MigoHud.Kind?
    private var expiry: DispatchWorkItem?

    init(presenter: MigoInteractionPresenter) {
        self.presenter = presenter
    }

    func handle(_ request: MigoHostServiceRequest) {
        let options = request.object
        switch request.method {
        case MigoHostServiceID.Interaction.showToast:
            let icon = options["icon"] as? String ?? "success"
            let duration = (options["duration"] as? NSNumber)?.doubleValue ?? 1500
            show(
                MigoHud(kind: .toast(icon: icon), title: options["title"] as? String ?? "",
                    mask: options["mask"] as? Bool ?? false),
                for: duration)
        case MigoHostServiceID.Interaction.hideToast:
            if case .toast = shown { hide() }
        case MigoHostServiceID.Interaction.showLoading:
            show(
                MigoHud(kind: .loading, title: options["title"] as? String ?? "",
                    mask: options["mask"] as? Bool ?? false),
                for: nil)
        case MigoHostServiceID.Interaction.hideLoading:
            if shown == .loading { hide() }
        case MigoHostServiceID.Interaction.showModal:
            let modal = MigoModal(
                title: options["title"] as? String ?? "", content: options["content"] as? String ?? "",
                showCancel: options["showCancel"] as? Bool ?? true,
                cancelText: options["cancelText"] as? String ?? Self.cancel,
                confirmText: options["confirmText"] as? String ?? Self.confirm,
                editable: options["editable"] as? Bool ?? false,
                placeholderText: options["placeholderText"] as? String ?? "")
            let reply = request.reply
            presenter.showModal(modal) { answer in
                guard let answer else {
                    reply?.fail("there is no window to show it in")
                    return
                }
                var result: [String: Any] = ["confirm": answer.confirmed, "cancel": !answer.confirmed]
                if modal.editable { result["content"] = answer.content ?? "" }
                reply?.succeed(result)
            }
        case MigoHostServiceID.Interaction.showActionSheet:
            let items = (options["itemList"] as? [Any] ?? []).compactMap { $0 as? String }
            guard !items.isEmpty else {
                request.reply?.fail("itemList is empty")
                return
            }
            let reply = request.reply
            presenter.showActionSheet(title: options["alertText"] as? String ?? "", items: items) { index in
                switch index {
                case nil: reply?.fail("there is no window to show it in")
                case let tapped? where tapped >= 0 && tapped < items.count: reply?.succeed(["tapIndex": tapped])
                default: reply?.fail("cancel")
                }
            }
        default:
            request.reply?.fail("not supported")
        }
    }

    private func show(_ hud: MigoHud, for durationMs: Double?) {
        expiry?.cancel()
        expiry = nil
        shown = hud.kind
        presenter.showHud(hud)
        guard let durationMs else { return }
        let work = DispatchWorkItem { [weak self] in self?.hide() }
        expiry = work
        DispatchQueue.main.asyncAfter(deadline: .now() + max(0, durationMs) / 1000, execute: work)
    }

    private func hide() {
        expiry?.cancel()
        expiry = nil
        shown = nil
        presenter.hideHud()
    }

    /// The platform's own defaults, which content's call carries anyway.
    static let cancel = "\u{53D6}\u{6D88}"
    static let confirm = "\u{786E}\u{5B9A}"
}
